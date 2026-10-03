//! Bash tool metadata, execution, and sed-edit application.
//!
//! Maps to: CC `tools/BashTool/BashTool.tsx` (`call`, `runShellCommand`,
//! `spawnBackgroundTask`, and `applySedEdit`). Rendering remains in `ui.rs`,
//! matching CC `tools/BashTool/UI.tsx`.

pub mod bash_command_helpers;
pub mod bash_permissions;
pub mod bash_security;
pub mod bash_tool_result_message;
pub mod command_semantics;
pub mod comment_label;
pub mod destructive_command_warning;
pub mod mode_validation;
pub mod path_validation;
pub mod prompt;
pub(crate) mod read_only_validation;
pub mod sed_edit_parser;
pub mod sed_validation;
pub mod should_use_sandbox;
pub mod tool_name;
pub mod ui;
pub mod utils;

const PROGRESS_THRESHOLD_MS: u64 = 2_000;
const ASSISTANT_BLOCKING_BUDGET_MS: u64 = 15_000;

const BASH_SEARCH_COMMANDS: &[&str] = &[
    "find", "grep", "rg", "ag", "ack", "locate", "which", "whereis",
];
const BASH_READ_COMMANDS: &[&str] = &[
    "cat", "head", "tail", "less", "more", "wc", "stat", "file", "strings", "jq", "awk", "cut",
    "sort", "uniq", "tr",
];
const BASH_LIST_COMMANDS: &[&str] = &["ls", "tree", "du"];
const BASH_SEMANTIC_NEUTRAL_COMMANDS: &[&str] = &["echo", "printf", "true", "false", ":"];
const BASH_SILENT_COMMANDS: &[&str] = &[
    "mv", "cp", "rm", "mkdir", "rmdir", "chmod", "chown", "chgrp", "touch", "ln", "cd", "export",
    "unset", "wait",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BashSearchReadKind {
    pub is_search: bool,
    pub is_read: bool,
    pub is_list: bool,
}

fn command_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && !single {
            escaped = true;
            current.push(ch);
            continue;
        }
        if ch == '\'' && !double {
            single = !single;
            continue;
        }
        if ch == '"' && !single {
            double = !double;
            continue;
        }
        if !single && !double && (ch.is_whitespace() || "|;&>".contains(ch)) {
            if !current.trim().is_empty() {
                words.push(std::mem::take(&mut current));
            }
            if "|;&>".contains(ch) {
                let mut operator = ch.to_string();
                if chars.peek().is_some_and(|next| {
                    (*next == ch && matches!(ch, '|' | '&' | '>')) || (ch == '>' && *next == '&')
                }) {
                    operator.push(chars.next().expect("peeked operator"));
                }
                words.push(operator);
            }
        } else {
            current.push(ch);
        }
    }
    if single || double {
        return Vec::new();
    }
    if !current.trim().is_empty() {
        words.push(current);
    }
    words
}

fn collapse_syntax_is_supported(command: &str) -> bool {
    let chars = command.chars().collect::<Vec<_>>();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if character == '\\' && !single {
            escaped = true;
            index += 1;
            continue;
        }
        if character == '\'' && !double {
            single = !single;
            index += 1;
            continue;
        }
        if character == '"' && !single {
            double = !double;
            index += 1;
            continue;
        }
        if !single && !double {
            if character == '$' && chars.get(index + 1) == Some(&'(') {
                return false;
            }
            if matches!(character, '<' | '>') && chars.get(index + 1) == Some(&'(') {
                return false;
            }
            if character == '&' {
                return false;
            }
            if character == '|' && chars.get(index + 1) == Some(&'&') {
                return false;
            }
            if character == '<' {
                if chars.get(index + 1) != Some(&'<') {
                    return false;
                }
                let rest = chars[index + 2..].iter().collect::<String>();
                let mut opening = rest.as_str();
                let strip_tabs = opening.starts_with('-');
                if strip_tabs {
                    opening = &opening[1..];
                }
                opening = opening.trim_start_matches([' ', '\t']);
                let (delimiter, consumed) = if let Some(value) = opening.strip_prefix('\'') {
                    let Some(end) = value.find('\'') else {
                        return false;
                    };
                    (&value[..end], end + 2)
                } else if let Some(value) = opening.strip_prefix('"') {
                    let Some(end) = value.find('"') else {
                        return false;
                    };
                    (&value[..end], end + 2)
                } else {
                    let value = opening.strip_prefix('\\').unwrap_or(opening);
                    let end = value
                        .find(|character: char| character.is_whitespace())
                        .unwrap_or(value.len());
                    (&value[..end], end + usize::from(opening.starts_with('\\')))
                };
                if delimiter.is_empty()
                    || !delimiter
                        .chars()
                        .all(|character| character == '_' || character.is_ascii_alphanumeric())
                {
                    return false;
                }
                let after_delimiter = &opening[consumed..];
                let Some(newline) = after_delimiter.find('\n') else {
                    return false;
                };
                if !after_delimiter[..newline].trim().is_empty() {
                    return false;
                }
                let body = &after_delimiter[newline + 1..];
                let mut offset = 0usize;
                for line in body.split_inclusive('\n') {
                    let raw = line.trim_end_matches('\n');
                    let comparable = if strip_tabs {
                        raw.trim_start_matches('\t')
                    } else {
                        raw
                    };
                    offset += line.len();
                    if comparable == delimiter {
                        // Commands after the closing marker require normal
                        // operator parsing; conservatively keep them expanded.
                        return body[offset..].trim().is_empty();
                    }
                }
                return false;
            }
        }
        index += 1;
    }
    true
}

/// Maps to CC `isSearchOrReadBashCommand(command)` (:198-266).
pub fn is_search_or_read_bash_command(command: &str) -> BashSearchReadKind {
    if !collapse_syntax_is_supported(command) {
        return BashSearchReadKind::default();
    }
    let parts = command_words(command);
    if parts.is_empty() {
        return BashSearchReadKind::default();
    }
    let mut result = BashSearchReadKind::default();
    let mut non_neutral = false;
    let mut command_position = true;
    let mut skip_redirect_target = false;
    for part in parts {
        if skip_redirect_target {
            skip_redirect_target = false;
            command_position = false;
            continue;
        }
        if matches!(part.as_str(), ">" | ">>" | ">&") {
            skip_redirect_target = true;
            continue;
        }
        if matches!(part.as_str(), "|" | ";" | "&&" | "||") {
            command_position = true;
            continue;
        }
        if !command_position {
            continue;
        }
        command_position = false;
        let base = part.split_whitespace().next().unwrap_or_default();
        if BASH_SEMANTIC_NEUTRAL_COMMANDS.contains(&base) {
            continue;
        }
        non_neutral = true;
        let search = BASH_SEARCH_COMMANDS.contains(&base);
        let read = BASH_READ_COMMANDS.contains(&base);
        let list = BASH_LIST_COMMANDS.contains(&base);
        if !search && !read && !list {
            return BashSearchReadKind::default();
        }
        result.is_search |= search;
        result.is_read |= read;
        result.is_list |= list;
    }
    non_neutral.then_some(result).unwrap_or_default()
}

fn is_silent_bash_command(command: &str) -> bool {
    if !collapse_syntax_is_supported(command) {
        return false;
    }
    let parts = command_words(command);
    if parts.is_empty() {
        return false;
    }
    let mut command_position = true;
    let mut last_was_or = false;
    let mut found = false;
    for part in parts {
        if matches!(part.as_str(), "|" | ";" | "&&" | "||") {
            command_position = true;
            last_was_or = part == "||";
            continue;
        }
        if matches!(part.as_str(), ">" | ">>" | ">&") {
            command_position = false;
            continue;
        }
        if !command_position {
            continue;
        }
        command_position = false;
        let base = part.split_whitespace().next().unwrap_or_default();
        if last_was_or && BASH_SEMANTIC_NEUTRAL_COMMANDS.contains(&base) {
            continue;
        }
        found = true;
        if !BASH_SILENT_COMMANDS.contains(&base) {
            return false;
        }
    }
    found
}

fn is_auto_backgrounding_allowed(command: &str) -> bool {
    let first = command_words(command)
        .into_iter()
        .take_while(|part| !matches!(part.as_str(), "|" | ";" | "&&" | "||" | ">" | ">>" | ">&"))
        .collect::<Vec<_>>();
    // CC compares the complete first `splitCommand_DEPRECATED` fragment to
    // `sleep`; consequently only a bare `sleep` (not `sleep 5`) is excluded.
    !(first.len() == 1 && first[0] == "sleep")
}

/// Prepare Bash hook `if` matching once per invocation. Maps to CC
/// `BashTool.preparePermissionMatcher` (`BashTool.tsx:663-681`). External CC
/// builds have no tree-sitter Bash parser, so `parse-unavailable` deliberately
/// fails safe by running every Bash-content hook. The internal projection uses
/// the statically recoverable argv subset and likewise fails safe for any shell
/// construct it cannot prove simple.
fn prepare_bash_permission_matcher(command: &str) -> crate::tool::PermissionPatternMatcher {
    if crate::utils::build_profile::build_audience().is_external() {
        return Box::new(|_| true);
    }

    let commands = match crate::utils::bash::ast::parse_for_security(command) {
        crate::utils::bash::ast::ParseForSecurityResult::Simple { commands }
            if !commands.is_empty() =>
        {
            commands
                .into_iter()
                .map(|command| command.argv.join(" "))
                .collect::<Vec<_>>()
        }
        crate::utils::bash::ast::ParseForSecurityResult::Simple { .. }
        | crate::utils::bash::ast::ParseForSecurityResult::ParseUnavailable
        | crate::utils::bash::ast::ParseForSecurityResult::TooComplex { .. } => {
            return Box::new(|_| true);
        }
    };

    Box::new(move |pattern| {
        let prefix = bash_permissions::permission_rule_extract_prefix(pattern);
        commands.iter().any(|command| {
            if let Some(prefix) = prefix {
                command == prefix || command.starts_with(&format!("{prefix} "))
            } else {
                bash_permissions::match_wildcard_pattern(pattern, command)
            }
        })
    })
}

/// Maps to CC `BashTool.inputSchema` internal `_simulatedSedEdit` payload.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SimulatedSedEdit {
    file_path: String,
    new_content: String,
}

/// Maps to: CC `BashTool.tsx:337-371` `fullInputSchema`.
///
/// Declaration order is CC's: `_simulatedSedEdit` sits last, and
/// `run_in_background` before `dangerouslyDisableSandbox` — `.omit()` preserves
/// the order of what it keeps.
fn full_input_fields() -> Vec<crate::utils::zod::ObjectField> {
    use crate::utils::zod;
    vec![
        ("command", zod::string().describe("The command to execute")),
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
            zod::string().optional().describe(
                "Clear, concise description of what this command does in active voice. Never use words like \"complex\" or \"risk\" in the description - just describe what it does.\n\nFor simple commands (git, npm, standard CLI tools), keep it brief (5-10 words):\n- ls → \"List files in current directory\"\n- git status → \"Show working tree status\"\n- npm install → \"Install package dependencies\"\n\nFor commands that are harder to parse at a glance (piped commands, obscure flags, etc.), add enough context to clarify what it does:\n- find . -name \"*.tmp\" -exec rm {} \\; → \"Find and delete all .tmp files recursively\"\n- git reset --hard origin/main → \"Discard all local changes and match remote main\"\n- curl -s url | jq '.data[]' → \"Fetch JSON from URL and extract data array elements\"",
            ),
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
        (
            "_simulatedSedEdit",
            zod::object(vec![
                ("filePath", zod::string()),
                ("newContent", zod::string()),
            ])
            .optional()
            .describe("Internal: pre-computed sed edit result from preview"),
        ),
    ]
}

/// Maps to: CC `BashTool.tsx:332-335` module-level `isBackgroundTasksDisabled`,
/// evaluated at import, before `init()` applies settings env (its eslint
/// exemption reads "Intentional: schema must be defined at module load").
/// `entrypoints/cli.rs` forces it in the startup window, so a value only
/// settings env sets is not seen here, as in CC; a mid-process change does not
/// re-shape the schema either.
pub(crate) static IS_BACKGROUND_TASKS_DISABLED: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| {
        crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS").as_deref(),
        )
    });

/// Maps to: CC `BashTool.tsx:378-385` `inputSchema`.
///
/// `_simulatedSedEdit` is ALWAYS omitted: it is set by SedEditPermissionRequest
/// after the user approves a preview, and exposing it would let the model pair
/// an innocuous command with an arbitrary file write, bypassing both the
/// permission check and the sandbox. `run_in_background` is additionally
/// omitted when background tasks are off ([`IS_BACKGROUND_TASKS_DISABLED`]).
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        let background_disabled = *IS_BACKGROUND_TASKS_DISABLED;
        // CC composes with `.omit()`; the contract puts composition at the
        // definition site, so the field list is filtered here instead.
        let fields = full_input_fields()
            .into_iter()
            .filter(|(name, _)| {
                *name != "_simulatedSedEdit"
                    && !(background_disabled && *name == "run_in_background")
            })
            .collect();
        crate::utils::zod::strict_object(fields)
    })
}

/// Maps to CC `BashTool.inputSchema`.
pub fn bash_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: tool_name::BASH_TOOL_NAME.to_string(),
        description: prompt::get_simple_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        strict: Some(true),
        ..Default::default()
    }
}

/// Maps to CC `tools/BashTool/BashTool.tsx:439-500` `outputSchema`.
#[derive(Clone, Debug, Default)]
pub(crate) struct BashOutput {
    /// CC `stdout` / `stderr`.
    /// For large outputs, `stdout` is the truncated preview shown in the UI;
    /// the full bytes live at `persisted_output_path` for the model/`Read`.
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    /// CC `interrupted` — abort/timeout kill. Drives `is_error` in map.
    pub(crate) interrupted: bool,
    /// CC `isImage` — stdout is a `data:image/...;base64,...` URI.
    pub(crate) is_image: bool,
    /// CC `structuredContent` — passthrough content blocks for map early-return.
    /// Bash `call` does not populate this (same as upstream); map still honours
    /// it when present (e.g. injected/replayed Out).
    pub(crate) structured_content: Option<Vec<serde_json::Value>>,
    /// CC `rawOutputPath` (reserved for externally supplied raw output).
    pub(crate) raw_output_path: Option<String>,
    /// CC `backgroundTaskId` and handoff causes.
    pub(crate) background_task_id: Option<String>,
    pub(crate) backgrounded_by_user: bool,
    pub(crate) assistant_auto_backgrounded: bool,
    pub(crate) dangerously_disable_sandbox: Option<bool>,
    pub(crate) no_output_expected: bool,
    /// CC `persistedOutputPath` — tool-results file with full stdout.
    pub(crate) persisted_output_path: Option<String>,
    /// CC `persistedOutputSize` — original full stdout byte length.
    pub(crate) persisted_output_size: Option<u64>,
    /// Exit status and semantic interpretation from `interpretCommandResult`.
    pub(crate) exit_code: Option<i32>,
    pub(crate) return_code_interpretation: Option<String>,
    /// Physical cwd reported by the shell trailer after a foreground command.
    pub(crate) cwd_after: Option<std::path::PathBuf>,
    /// Rust seam: the executed command for the map/tracking boundary (CC UI
    /// takes it from the tool_use input); never rides the wire.
    pub(crate) command: String,
}

fn simulated_sed_edit_from_args(args: &serde_json::Value) -> Option<SimulatedSedEdit> {
    let simulated = args.get("_simulatedSedEdit")?;
    Some(SimulatedSedEdit {
        file_path: simulated.get("filePath")?.as_str()?.to_string(),
        new_content: simulated.get("newContent")?.as_str()?.to_string(),
    })
}

/// Maps to: CC `tools/BashTool/BashTool.tsx` `applySedEdit` (:575).
///
/// The approved preview is written with the original encoding/line endings;
/// file history, VS Code, and `readFileState` are updated here. No shell process
/// is involved, so the bytes applied are exactly the bytes the permission UI
/// approved.
async fn apply_simulated_sed_edit(
    simulated: &SimulatedSedEdit,
    command: &str,
    context: &crate::tool::ToolUseContext,
    parent_message: Option<&crate::types::message::AssistantMessage>,
) -> Result<BashOutput, String> {
    if !crate::utils::session_storage::is_session_write_enabled() {
        return Err(crate::tools::shared::write_gate::SED_EDIT_DISABLED_ERROR.to_string());
    }
    let path = crate::utils::path::expand_path(
        &simulated.file_path,
        Some(context.effective_cwd().as_path()),
    )?;
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let encoding = crate::utils::file::detect_file_encoding(&path);
    let original = match fs.read_file(&path, encoding.into()).await {
        Ok(content) => content.to_string_lossy(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BashOutput {
                stderr: format!(
                    "sed: {}: No such file or directory\nExit code 1",
                    simulated.file_path
                ),
                exit_code: Some(1),
                return_code_interpretation: Some("Command failed with exit code 1".to_string()),
                command: command.to_string(),
                ..Default::default()
            });
        }
        Err(error) => return Err(format!("Error running command: {error}")),
    };

    if let (Some(parent), Some(store)) = (parent_message, context.app_store.store.as_ref()) {
        if crate::utils::file_history::file_history_enabled() {
            crate::utils::file_history::file_history_track_edit(
                store,
                &path.display().to_string(),
                // The ENVELOPE uuid. CC passes `parentMessage.uuid`
                // (`FileWriteTool.ts:261`, `NotebookEditTool.ts:315`), and
                // `fileHistoryRewind` looks the snapshot up by that same value
                // (`fileHistory.ts:367`). This used to pass the identity
                // block's own uuid, which no other consumer holds.
                &parent.uuid,
            )
            .await;
        }
    }
    let endings = crate::utils::file::detect_line_endings(&path, encoding);
    crate::utils::file::write_text_content(&path, &simulated.new_content, encoding, endings)
        .map_err(|error| format!("Error running command: {error}"))?;

    let _ = crate::services::mcp::vscode_sdk_mcp::notify_vscode_file_updated(
        &path.display().to_string(),
        Some(&original),
        Some(&simulated.new_content),
    )
    .await;
    // Maps to: CC `BashTool.tsx:617-623` `applySedEdit`
    // `readFileState.set(absoluteFilePath, ...)`.
    context
        .read_file_state
        .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
            path: path.display().to_string(),
            content: Some(simulated.new_content.clone()),
            timestamp_ms: crate::utils::file::get_file_modification_time(&path),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: crate::utils::query_helpers::ReadFileStateSource::EditRefresh,
        });
    Ok(BashOutput {
        exit_code: Some(0),
        command: command.to_string(),
        no_output_expected: true,
        ..Default::default()
    })
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BashProgressData {
    output: String,
    full_output: String,
    elapsed_time_seconds: u64,
    total_lines: usize,
    total_bytes: Option<u64>,
    task_id: Option<String>,
    timeout_ms: Option<u64>,
}

fn task_store(context: &crate::tool::ToolUseContext) -> Option<crate::state::store::AppStore> {
    context
        .app_store
        .tasks_store
        .clone()
        .or_else(|| context.app_store.store.clone())
}

enum BackgroundStart {
    Started(String),
    AlreadySettled,
}

/// Maps to CC `runShellCommand` nested `spawnBackgroundTask()`.
fn spawn_background_task(
    command: &str,
    description: &str,
    shell_command: &crate::utils::shell_command::ShellCommand,
    context: &crate::tool::ToolUseContext,
    tool_use_id: Option<&str>,
) -> Result<String, String> {
    let store = task_store(context)
        .ok_or_else(|| "Background command requires an AppState task store".to_string())?;
    crate::tasks::local_shell_task::spawn_shell_task(
        crate::tasks::local_shell_task::LocalShellSpawnInput {
            command: command.to_string(),
            description: description.to_string(),
            shell_command: shell_command.clone(),
            tool_use_id: tool_use_id.map(ToOwned::to_owned),
            agent_id: context.agent_id.clone(),
            kind: None,
        },
        &store,
    )
}

/// Maps to CC `runShellCommand` nested `startBackgrounding(...)`.
fn start_backgrounding(
    command: &str,
    description: &str,
    shell_command: &crate::utils::shell_command::ShellCommand,
    context: &crate::tool::ToolUseContext,
    tool_use_id: Option<&str>,
    foreground_task_id: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
    by_user: bool,
) -> Result<BackgroundStart, String> {
    if let Some(id) = foreground_task_id.lock().ok().and_then(|id| id.clone()) {
        return Ok(
            if crate::tasks::local_shell_task::background_existing_foreground_task(&id, by_user) {
                BackgroundStart::Started(id)
            } else {
                // A failed in-place transition means the command settled in the
                // race window. Never re-register it as a second task.
                BackgroundStart::AlreadySettled
            },
        );
    }
    spawn_background_task(command, description, shell_command, context, tool_use_id)
        .map(BackgroundStart::Started)
}

/// Maps to CC `runShellCommand(...)` (`BashTool.tsx:1104-1483`).
fn run_shell_command(
    args: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
    tool_use_id: Option<&str>,
    mut on_progress: Option<&mut dyn FnMut(BashProgressData)>,
) -> Result<crate::utils::shell_command::ExecResult, String> {
    use crate::utils::shell::shell_provider::ShellType;
    use crate::utils::shell_command::ShellCommandStatus;

    let command = args
        .get("command")
        .and_then(serde_json::Value::as_str)
        .or_else(|| args.get("value").and_then(serde_json::Value::as_str))
        .unwrap_or_default();
    if command.is_empty() {
        return Err("Error running command: missing command".to_string());
    }
    let description = args
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(command);
    let timeout_ms = crate::utils::shell::shell_timeout_ms(args);
    let run_in_background = args
        .get("run_in_background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if run_in_background && !crate::utils::session_storage::is_session_write_enabled() {
        return Err(
            crate::tools::shared::write_gate::BACKGROUND_COMMAND_DISABLED_ERROR.to_string(),
        );
    }
    // CC `BashTool.tsx:1159` reads the module-level constant.
    let background_disabled = *IS_BACKGROUND_TASKS_DISABLED;
    let should_auto_background = !background_disabled
        && is_auto_backgrounding_allowed(command)
        && task_store(context).is_some()
        && crate::utils::session_storage::is_session_write_enabled();
    let dangerously_disable_sandbox = args
        .get("dangerouslyDisableSandbox")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let use_sandbox = should_use_sandbox::should_use_sandbox(&should_use_sandbox::SandboxInput {
        command: Some(command.to_string()),
        dangerously_disable_sandbox,
    });
    let shell_command = crate::utils::shell::exec(
        command,
        &context.abort_controller,
        ShellType::Bash,
        crate::utils::shell::ExecOptions {
            timeout: Some(std::time::Duration::from_millis(timeout_ms)),
            cwd: Some(context.effective_cwd()),
            prevent_cwd_changes: context.agent_id.is_some(),
            should_use_sandbox: use_sandbox,
            should_auto_background,
        },
    )?;

    let foreground_task_id = std::sync::Arc::new(std::sync::Mutex::new(None));
    let background_task_id = std::sync::Arc::new(std::sync::Mutex::new(None));
    if should_auto_background {
        let command = command.to_string();
        let description = description.to_string();
        let shell = shell_command.clone();
        let context = context.clone();
        let tool_use_id = tool_use_id.map(ToOwned::to_owned);
        let foreground = std::sync::Arc::clone(&foreground_task_id);
        let background = std::sync::Arc::clone(&background_task_id);
        shell_command.on_timeout(move || {
            match start_backgrounding(
                &command,
                &description,
                &shell,
                &context,
                tool_use_id.as_deref(),
                &foreground,
                false,
            ) {
                Ok(BackgroundStart::Started(id)) => {
                    if let Ok(mut slot) = background.lock() {
                        *slot = Some(id);
                    }
                }
                Ok(BackgroundStart::AlreadySettled) => {}
                Err(_) => shell.kill(),
            }
        });
    }

    if run_in_background && !background_disabled {
        let id = spawn_background_task(command, description, &shell_command, context, tool_use_id)?;
        return Ok(crate::utils::shell_command::ExecResult {
            background_task_id: Some(id),
            ..Default::default()
        });
    }

    let started = std::time::Instant::now();
    if let Some(result) =
        shell_command.wait_result_timeout(std::time::Duration::from_millis(PROGRESS_THRESHOLD_MS))
    {
        shell_command.cleanup();
        return Ok(result);
    }
    if let Some(id) = background_task_id.lock().ok().and_then(|id| id.clone()) {
        return Ok(crate::utils::shell_command::ExecResult {
            background_task_id: Some(id),
            ..Default::default()
        });
    }

    // TaskOutput's shared poller starts after the 2s threshold and ticks once
    // per second; the first progress sample therefore occurs on the next tick.
    let mut last_progress = std::time::Instant::now();
    let mut assistant_auto_backgrounded = false;
    loop {
        if let Some(mut result) =
            shell_command.wait_result_timeout(std::time::Duration::from_millis(50))
        {
            if result.background_task_id.is_some() {
                if let Some(id) = result.background_task_id.as_deref() {
                    crate::tasks::local_shell_task::mark_task_notified(id);
                }
                result.background_task_id = None;
                if shell_command.task_output().stdout_to_file()
                    && !shell_command.task_output().output_file_redundant()
                {
                    result.output_file_path =
                        Some(shell_command.task_output().path().display().to_string());
                    result.output_file_size = Some(shell_command.task_output().output_file_size());
                    result.output_task_id = Some(shell_command.task_output().task_id().to_string());
                }
            } else if let Some(id) = foreground_task_id.lock().ok().and_then(|id| id.clone()) {
                crate::tasks::local_shell_task::unregister_foreground(&id);
            }
            shell_command.cleanup();
            return Ok(result);
        }

        if shell_command.status() == ShellCommandStatus::Backgrounded {
            let id = background_task_id
                .lock()
                .ok()
                .and_then(|id| id.clone())
                .unwrap_or_else(|| shell_command.task_output().task_id().to_string());
            return Ok(crate::utils::shell_command::ExecResult {
                background_task_id: Some(id),
                backgrounded_by_user: shell_command.was_backgrounded_by_user(),
                assistant_auto_backgrounded: shell_command.was_assistant_auto_backgrounded(),
                ..Default::default()
            });
        }

        let elapsed = started.elapsed();
        let kairos_auto_background = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::Kairos,
        ) && crate::bootstrap::state::get_kairos_active()
            && context.agent_id.is_none()
            && !background_disabled
            && crate::utils::session_storage::is_session_write_enabled()
            && !run_in_background
            && elapsed >= std::time::Duration::from_millis(ASSISTANT_BLOCKING_BUDGET_MS);
        if kairos_auto_background && !assistant_auto_backgrounded {
            assistant_auto_backgrounded = true;
            match start_backgrounding(
                command,
                description,
                &shell_command,
                context,
                tool_use_id,
                &foreground_task_id,
                false,
            )? {
                BackgroundStart::Started(id) => {
                    shell_command.mark_assistant_auto_backgrounded();
                    if let Ok(mut slot) = background_task_id.lock() {
                        *slot = Some(id);
                    }
                }
                BackgroundStart::AlreadySettled => {}
            }
            continue;
        }

        if last_progress.elapsed() >= std::time::Duration::from_secs(1) {
            last_progress = std::time::Instant::now();
            if on_progress.is_some()
                && !background_disabled
                && crate::utils::session_storage::is_session_write_enabled()
                && foreground_task_id.lock().is_ok_and(|id| id.is_none())
            {
                if let Some(store) = task_store(context) {
                    let id = crate::tasks::local_shell_task::register_foreground(
                        crate::tasks::local_shell_task::LocalShellSpawnInput {
                            command: command.to_string(),
                            description: description.to_string(),
                            shell_command: shell_command.clone(),
                            tool_use_id: tool_use_id.map(ToOwned::to_owned),
                            agent_id: context.agent_id.clone(),
                            kind: None,
                        },
                        &store,
                    );
                    if let Ok(mut slot) = foreground_task_id.lock() {
                        *slot = Some(id);
                    }
                }
            }
            let progress = shell_command.task_output().poll_progress();
            if let Some(callback) = on_progress.as_deref_mut() {
                callback(BashProgressData {
                    output: progress.last_lines,
                    full_output: progress.all_lines,
                    elapsed_time_seconds: elapsed.as_secs(),
                    total_lines: progress.total_lines,
                    total_bytes: progress.is_incomplete.then_some(progress.total_bytes),
                    task_id: Some(shell_command.task_output().task_id().to_string()),
                    timeout_ms: args.get("timeout").is_some().then_some(timeout_ms),
                });
            }
        }
    }
}

/// Testable Rust extraction of the large-output persistence block inside CC
/// `BashTool.call` (`BashTool.tsx:956-980`); it stays in the BashTool owner
/// because upstream does not define a separate output-persistence module.
fn persist_output_file(source: &str, task_id: &str, original_size: u64) -> Option<String> {
    use std::io::{Read as _, Seek as _, Write as _};

    const MAX_PERSISTED_SIZE: u64 = 64 * 1024 * 1024;
    if !crate::utils::session_storage::is_session_write_enabled() {
        return None;
    }
    crate::utils::tool_result_storage::ensure_tool_results_dir();
    let destination = crate::utils::tool_result_storage::get_tool_result_path(task_id, false);
    let source_path = std::path::Path::new(source);
    // The child received an O_NOFOLLOW descriptor, but can rename the pathname
    // and plant a symlink before this host-side persistence step. Hold a verified
    // regular-file handle for both truncation and copy so that path replacement
    // cannot redirect either operation to an arbitrary host file.
    let mut source_file =
        crate::utils::fs_operations::open_regular_file_no_follow(source_path, true).ok()?;
    if original_size > MAX_PERSISTED_SIZE {
        source_file.set_len(MAX_PERSISTED_SIZE).ok()?;
    }
    source_file.rewind().ok()?;

    let parent = destination.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    let temporary = parent.join(format!(
        ".{}-{}.tmp",
        task_id,
        uuid::Uuid::new_v4().simple()
    ));
    let copied = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut output = options.open(&temporary)?;
        let mut bounded = source_file.take(MAX_PERSISTED_SIZE);
        std::io::copy(&mut bounded, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        crate::utils::fs_operations::replace_file_atomic(&temporary, &destination)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if copied.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return None;
    }
    Some(destination.display().to_string())
}

/// Execute a permitted Bash tool use, producing the CC-shaped output.
/// Maps to CC `BashTool.call` (`BashTool.tsx:846-1098`).
pub(crate) fn bash_output(
    args: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
    tool_use_id: Option<&str>,
    on_progress: Option<&mut dyn FnMut(BashProgressData)>,
) -> Result<BashOutput, String> {
    let command = args
        .get("command")
        .and_then(serde_json::Value::as_str)
        .or_else(|| args.get("value").and_then(serde_json::Value::as_str))
        .unwrap_or_default();
    let result = run_shell_command(args, context, tool_use_id, on_progress)?;
    if let Some(error) = result.pre_spawn_error.as_deref() {
        return Err(error.to_string());
    }
    crate::tools::shared::git_operation_tracking::track_git_operations(
        command,
        result.code,
        Some(&result.stdout),
    );
    let is_interrupt =
        result.interrupted && context.abort_controller.reason().as_deref() == Some("interrupt");
    let mut full_stdout = result.stdout.clone();
    // Production Bash uses one file descriptor for stdout+stderr. The explicit
    // COMETIX_WRITE_ENABLED=0 foreground fallback cannot create that artifact,
    // so ShellCommand exposes only the raw child stderr (excluding synthetic
    // timeout/watchdog diagnostics) for bounded model-visible merging here.
    if let Some(pipe_stderr) = result.pipe_stderr.as_deref() {
        full_stdout.push_str(pipe_stderr);
    }
    let interpretation =
        command_semantics::interpret_command_result(command, result.code, &full_stdout, "");
    let sandboxed = should_use_sandbox::should_use_sandbox(&should_use_sandbox::SandboxInput {
        command: Some(command.to_string()),
        dangerously_disable_sandbox: args
            .get("dangerouslyDisableSandbox")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    });
    if sandboxed && interpretation.is_error && !is_interrupt {
        full_stdout = crate::utils::sandbox::sandbox_adapter::annotate_stderr_with_sandbox_failures(
            command,
            &full_stdout,
        );
    }
    crate::utils::sandbox::sandbox_adapter::record_sandbox_violations_from_stderr(&full_stdout);
    if interpretation.is_error && !is_interrupt && result.code != 0 {
        if !full_stdout.is_empty() && !full_stdout.ends_with('\n') {
            full_stdout.push('\n');
        }
        full_stdout.push_str(&format!("Exit code {}", result.code));
    }
    let (cwd_after, cwd_was_reset) = if context.agent_id.is_none() {
        utils::reset_cwd_if_outside_project(result.cwd_after.clone(), context)
    } else {
        (None, false)
    };
    // Bash file mode has one merged fd. ShellCommand's synthetic timeout /
    // watchdog stderr is not part of CC's thrown ShellError; model mapping
    // derives `Exit code N` and uses the merged output instead.
    let mut stderr = if result.pipe_stderr.is_some() || (interpretation.is_error && !is_interrupt) {
        String::new()
    } else {
        result.stderr
    };
    if cwd_was_reset && !(interpretation.is_error && !is_interrupt) {
        stderr = utils::stderr_append_shell_reset_message(&stderr);
    }

    let mut stripped = utils::strip_empty_lines(&full_stdout);
    let extracted = crate::utils::claude_code_hints::extract_claude_code_hints(&stripped, command);
    stripped = extracted.stripped;
    if context.agent_id.is_none() {
        for hint in extracted.hints {
            crate::utils::plugins::hint_recommendation::maybe_record_plugin_hint(hint);
        }
    }

    let persisted_output_size = result.output_file_size;
    let persisted_output_path = if interpretation.is_error && !is_interrupt {
        None
    } else {
        match (
            result.output_file_path.as_deref(),
            result.output_task_id.as_deref(),
            result.output_file_size,
        ) {
            (Some(path), Some(task_id), Some(size)) => persist_output_file(path, task_id, size),
            _ => None,
        }
    };

    let mut is_image = utils::is_image_output(&stripped);
    if is_image {
        match utils::resize_shell_image_output(
            &stripped,
            result.output_file_path.as_deref(),
            result.output_file_size,
        ) {
            Some(resized) => stripped = resized,
            None => is_image = false,
        }
    }
    if !is_image && persisted_output_path.is_none() {
        stripped = utils::format_output(&stripped).truncated_content;
    }

    Ok(BashOutput {
        stdout: stripped,
        stderr,
        interrupted: result.interrupted,
        is_image,
        structured_content: None,
        raw_output_path: None,
        background_task_id: result.background_task_id,
        backgrounded_by_user: result.backgrounded_by_user,
        assistant_auto_backgrounded: result.assistant_auto_backgrounded,
        dangerously_disable_sandbox: args
            .get("dangerouslyDisableSandbox")
            .and_then(serde_json::Value::as_bool),
        no_output_expected: is_silent_bash_command(command),
        persisted_output_path,
        persisted_output_size,
        exit_code: Some(result.code),
        return_code_interpretation: interpretation.message,
        cwd_after,
        command: command.to_string(),
    })
}

/// Behavioral half of CC `BashTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct BashTool;

impl crate::tool::ToolCall for BashTool {
    fn name(&self) -> &'static str {
        "Bash"
    }

    /// Maps to: CC `BashTool.tsx:649-651` `async prompt() { return
    /// getSimplePrompt() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_simple_prompt()
    }

    /// Maps to: CC `BashTool.tsx:337-363` Zod semantic preprocessors.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut parsed = args.clone();
        crate::utils::semantic_number::preprocess_object_field(&mut parsed, "timeout");
        for field in ["run_in_background", "dangerouslyDisableSandbox"] {
            crate::utils::semantic_boolean::preprocess_object_field(&mut parsed, field);
        }
        parsed
    }

    /// Maps to: CC `BashTool.isConcurrencySafe(input)` (BashTool.tsx:652)
    /// delegating to `isReadOnly(input)` (:655) →
    /// `checkReadOnlyConstraints(...).behavior === 'allow'`.
    fn is_concurrency_safe(&self, args: &serde_json::Value) -> bool {
        self.is_read_only(args)
    }

    /// Maps to: CC `BashTool.isReadOnly(input)` → read-only constraints allow.
    fn is_read_only(&self, args: &serde_json::Value) -> bool {
        args.get("command")
            .and_then(|value| value.as_str())
            .is_some_and(|command| {
                read_only_validation::check_read_only_constraints(
                    command,
                    &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                )
            })
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("execute shell commands")
    }

    /// Maps to: CC `BashTool.description({ description })` (BashTool.tsx:646-648).
    /// Maps to: CC `BashTool.tsx:720-729` `getToolUseSummary(input)` —
    /// no command → null; a truthy description wins; otherwise the
    /// truncated command.
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

    fn description(&self, args: &serde_json::Value) -> String {
        args.get("description")
            .and_then(serde_json::Value::as_str)
            .filter(|description| !description.is_empty())
            .unwrap_or("Run shell command")
            .to_string()
    }

    fn max_result_size_chars(&self) -> usize {
        30_000
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn prepare_permission_matcher(
        &self,
        args: &serde_json::Value,
    ) -> Option<crate::tool::PermissionPatternMatcher> {
        Some(prepare_bash_permission_matcher(
            args.get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        ))
    }

    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let command = args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let mut ast_commands = None;
        if crate::utils::build_profile::build_audience().is_internal()
            && !crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_DISABLE_COMMAND_INJECTION_CHECK")
                    .as_deref(),
            )
        {
            match crate::utils::bash::ast::parse_for_security(command) {
                crate::utils::bash::ast::ParseForSecurityResult::TooComplex { reason, .. } => {
                    return bash_permissions::ast_security_result(
                        command,
                        &context.tool_permission_context,
                        &reason,
                        &[],
                    );
                }
                crate::utils::bash::ast::ParseForSecurityResult::Simple { commands } => {
                    if let crate::utils::bash::ast::SemanticCheckResult::Unsafe { reason } =
                        crate::utils::bash::ast::check_semantics(&commands)
                    {
                        return bash_permissions::ast_security_result(
                            command,
                            &context.tool_permission_context,
                            &reason,
                            &commands,
                        );
                    }
                    ast_commands = Some(commands);
                }
                crate::utils::bash::ast::ParseForSecurityResult::ParseUnavailable => {}
            }
        }
        let settings = crate::utils::settings::get_initial_settings();
        if crate::utils::sandbox::sandbox_adapter::is_auto_allow_bash_if_sandboxed_enabled(
            &settings,
        ) && !bash_permissions::has_obscured_command_names(command)
            && should_use_sandbox::should_use_sandbox(&should_use_sandbox::SandboxInput {
                command: Some(command.to_string()),
                dangerously_disable_sandbox: args
                    .get("dangerouslyDisableSandbox")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        {
            return match ast_commands.as_deref() {
                Some(commands) => bash_permissions::check_sandbox_auto_allow_from_ast(
                    command,
                    commands,
                    &context.tool_permission_context,
                ),
                None => bash_permissions::check_sandbox_auto_allow(
                    command,
                    &context.tool_permission_context,
                ),
            };
        }
        let cwd = context.effective_cwd();
        if let Some(commands) = ast_commands.as_deref() {
            return bash_permissions::bash_tool_has_permission_from_ast(
                command,
                commands,
                &context.tool_permission_context,
                &cwd,
            );
        }
        bash_permissions::bash_tool_has_permission(command, &context.tool_permission_context, &cwd)
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        parent_message: Option<&'a crate::types::message::AssistantMessage>,
        on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            if let Some(simulated) = simulated_sed_edit_from_args(args) {
                return match apply_simulated_sed_edit(
                    &simulated,
                    args.get("command")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                    context,
                    parent_message,
                )
                .await
                {
                    Ok(output) => crate::tool::ToolResult {
                        data: crate::tool::ToolOutput::Bash(output),
                        new_messages: Vec::new(),
                    },
                    Err(message) => crate::tool::ToolResult {
                        data: crate::tool::ToolOutput::Composed {
                            content: message,
                            status: crate::types::message::ToolResultStatus::Error,
                        },
                        new_messages: Vec::new(),
                    },
                };
            }

            let mut forward = on_progress.map(|progress| {
                move |update: BashProgressData| {
                    progress(crate::types::tools::ToolProgress::BashProgress {
                        tool_use_id: crate::types::ids::ToolUseId(request.tool_use_id.clone()),
                        output: update.output,
                        full_output: update.full_output,
                        elapsed_time_seconds: update.elapsed_time_seconds,
                        total_lines: update.total_lines,
                        total_bytes: update.total_bytes,
                        task_id: update.task_id,
                        timeout_ms: update.timeout_ms,
                    });
                }
            });
            match bash_output(
                args,
                context,
                Some(&request.tool_use_id),
                forward
                    .as_mut()
                    .map(|callback| callback as &mut dyn FnMut(BashProgressData)),
            ) {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Bash(output),
                    new_messages: Vec::new(),
                },
                Err(message) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Composed {
                        content: message,
                        status: crate::types::message::ToolResultStatus::Error,
                    },
                    new_messages: Vec::new(),
                },
            }
        })
    }

    /// Maps to: CC `tools/BashTool/BashTool.tsx`
    /// `mapToolResultToToolResultBlockParam` (:768) — structuredContent /
    /// isImage early returns, leading-blank-line strip + trimEnd on stdout,
    /// `<persisted-output>` wrapper for large stdout, interrupt error tag,
    /// background handoff copy, and `is_error: interrupted` (execution errors
    /// are surfaced as error results by this Rust call boundary).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::ToolResultStatus;
        match data {
            crate::tool::ToolOutput::Bash(output) => {
                let semantic_error = !output.interrupted
                    && output.exit_code.is_some_and(|exit_code| {
                        command_semantics::interpret_command_result(
                            &output.command,
                            exit_code,
                            &output.stdout,
                            &output.stderr,
                        )
                        .is_error
                    });
                let status = if output.interrupted || semantic_error {
                    ToolResultStatus::Error
                } else {
                    ToolResultStatus::Success
                };

                // CC: structuredContent short-circuits to content array.
                // Model path attaches blocks via `model_content_blocks_from_tool_output`;
                // empty string here is rewritten to the no-output marker then
                // cleared when blocks attach.
                if output
                    .structured_content
                    .as_ref()
                    .is_some_and(|blocks| !blocks.is_empty())
                {
                    return (String::new(), status);
                }

                // CC: isImage → image content block for Claude. Transcript UI
                // keeps this label (display stdout is cleared); model path
                // clears content and attaches the image block.
                if output.is_image {
                    if crate::tools::bash_tool::utils::build_image_tool_result(
                        &output.stdout,
                        _tool_use_id,
                    )
                    .is_some()
                    {
                        return (
                            "[Image data detected and sent to Claude]".to_string(),
                            status,
                        );
                    }
                    // Parse failed — fall through to text like CC's defensive path.
                }

                let mut processed_stdout = output.stdout.clone();
                if !processed_stdout.is_empty() {
                    while let Some(rest) = processed_stdout
                        .split_once('\n')
                        .filter(|(first, _)| first.trim().is_empty())
                        .map(|(_, rest)| rest.to_string())
                    {
                        processed_stdout = rest;
                    }
                    processed_stdout = processed_stdout.trim_end().to_string();
                }

                // UI keeps truncated stdout; model sees <persisted-output> + path.
                if let Some(path) = output.persisted_output_path.as_deref() {
                    let (preview, has_more) = crate::utils::tool_result_storage::generate_preview(
                        &processed_stdout,
                        crate::utils::tool_result_storage::PREVIEW_SIZE_BYTES,
                    );
                    processed_stdout =
                        crate::utils::tool_result_storage::build_large_tool_result_message(
                            &crate::utils::tool_result_storage::PersistedToolResult {
                                filepath: std::path::PathBuf::from(path),
                                original_size: output
                                    .persisted_output_size
                                    .unwrap_or(processed_stdout.len() as u64)
                                    as usize,
                                is_json: false,
                                preview,
                                has_more,
                            },
                        );
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
                    .map(|task_id| {
                        let path = crate::utils::task::disk_output::get_task_output_path(task_id);
                        if output.assistant_auto_backgrounded {
                            format!("Command exceeded the assistant-mode blocking budget ({}s) and was moved to the background with ID: {task_id}. It is still running — you will be notified when it completes. Output is being written to: {}. In assistant mode, delegate long-running work to a subagent or use run_in_background to keep this conversation responsive.", ASSISTANT_BLOCKING_BUDGET_MS / 1000, path.display())
                        } else if output.backgrounded_by_user {
                            format!("Command was manually backgrounded by user with ID: {task_id}. Output is being written to: {}", path.display())
                        } else {
                            format!("Command running in background with ID: {task_id}. Output is being written to: {}", path.display())
                        }
                    })
                    .unwrap_or_default();

                let content = if semantic_error {
                    // CC throws ShellError for semantic failures; formatError
                    // orders exit code, interrupt marker, merged output.
                    let mut exit = output
                        .exit_code
                        .filter(|code| *code != 0)
                        .map(|code| format!("Exit code {code}"))
                        .unwrap_or_default();
                    if !exit.is_empty() && error_message.lines().any(|line| line == exit) {
                        exit.clear();
                    }
                    if !exit.is_empty() {
                        if processed_stdout == exit {
                            processed_stdout.clear();
                        } else if processed_stdout
                            .strip_suffix(&exit)
                            .is_some_and(|prefix| prefix.ends_with('\n'))
                        {
                            processed_stdout.truncate(processed_stdout.len() - exit.len() - 1);
                        }
                    }
                    [exit, error_message, processed_stdout, background_info]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    [processed_stdout, error_message, background_info]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                (content, status)
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (String::new(), ToolResultStatus::Error),
        }
    }

    /// Maps to: CC recording BashTool's `Out` as the message's
    /// `toolUseResult` — the render layer parses it back with
    /// `ui::parse_output` and routes it through `BashToolResultMessage`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::Bash(output) => Some(ui::output_to_value(output)),
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

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn internal_permission_path_uses_ast_and_preserves_deny_precedence() {
        use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};

        let tool = BashTool;
        let context = crate::tool::ToolUseContext::default();
        assert!(matches!(
            crate::tool::ToolCall::check_permissions(
                &tool,
                &serde_json::json!({"command": "cat $(printf /etc/passwd)"}),
                &context,
            ),
            crate::utils::permissions::permission_result::PermissionResult::Ask { .. }
        ));

        let mut denied = crate::tool::ToolUseContext::default();
        denied.tool_permission_context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", Some("eval:*".to_string()))],
        );
        assert!(matches!(
            crate::tool::ToolCall::check_permissions(
                &tool,
                &serde_json::json!({"command": "printf x | eval 'echo bad'"}),
                &denied,
            ),
            crate::utils::permissions::permission_result::PermissionResult::Deny { .. }
        ));

        let outside = std::env::temp_dir().join(format!(
            "cometix-internal-ast-redirection-{}",
            uuid::Uuid::new_v4().simple()
        ));
        assert!(matches!(
            crate::tool::ToolCall::check_permissions(
                &tool,
                &serde_json::json!({"command": format!("echo x > {}", outside.display()), "dangerouslyDisableSandbox": true}),
                &context,
            ),
            crate::utils::permissions::permission_result::PermissionResult::Ask { .. }
        ));
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn sandbox_permission_precedence_resists_control_flow_dynamic_names_and_override_bypass() {
        use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};
        use crate::utils::permissions::permission_result::{
            PermissionDecisionReason, PermissionResult,
        };

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-sandbox-permission-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        std::fs::write(
            root.join("settings.json"),
            r#"{"sandbox":{"enabled":true,"autoAllowBashIfSandboxed":true,"allowUnsandboxedCommands":true}}"#,
        )
        .unwrap();

        let mut context = crate::tool::ToolUseContext::default();
        context.tool_permission_context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("git push:*".to_string()),
            )],
        );
        context.tool_permission_context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", Some("rm:*".to_string()))],
        );
        let tool = BashTool;
        assert!(matches!(
            crate::tool::ToolCall::check_permissions(
                &tool,
                &serde_json::json!({"command": "if true; then rm -f file; fi"}),
                &context,
            ),
            PermissionResult::Ask { .. }
        ));
        assert!(matches!(
            crate::tool::ToolCall::check_permissions(
                &tool,
                &serde_json::json!({"command": "printf x | git push origin main"}),
                &context,
            ),
            PermissionResult::Deny { .. }
        ));
        assert!(matches!(
            crate::tool::ToolCall::check_permissions(
                &tool,
                &serde_json::json!({"command": "\"$CMD\" status"}),
                &crate::tool::ToolUseContext::default(),
            ),
            PermissionResult::Ask { .. }
        ));

        let unsandboxed = crate::tool::ToolCall::check_permissions(
            &tool,
            &serde_json::json!({
                "command": "cargo build",
                "dangerouslyDisableSandbox": true
            }),
            &crate::tool::ToolUseContext::default(),
        );
        assert!(!matches!(
            unsandboxed,
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::Other { ref reason }),
                ..
            } if reason.contains("Auto-allowed with sandbox")
        ));

        std::fs::write(
            root.join("settings.json"),
            r#"{"sandbox":{"enabled":true,"autoAllowBashIfSandboxed":true,"allowUnsandboxedCommands":false}}"#,
        )
        .unwrap();
        // `settings_cache` is process-global like CC's module scope, and its own
        // doc states the contract: "Tests that mutate settings on disk must call
        // reset_settings_cache". Without it the second read still sees
        // `allowUnsandboxedCommands: true` from the first write, so the override
        // this test exists to prove is resisted looks like it is honoured.
        crate::utils::settings::settings_cache::reset_settings_cache();
        assert!(matches!(
            crate::tool::ToolCall::check_permissions(
                &tool,
                &serde_json::json!({
                    "command": "cargo build",
                    "dangerouslyDisableSandbox": true
                }),
                &crate::tool::ToolUseContext::default(),
            ),
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::Other { ref reason }),
                ..
            } if reason.contains("Auto-allowed with sandbox")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bash_read_search_classification_rejects_executable_shell_structure() {
        assert_eq!(
            is_search_or_read_bash_command("cat file | head -n 1"),
            BashSearchReadKind {
                is_read: true,
                ..Default::default()
            }
        );
        for command in [
            "cat $(printf file)",
            "cat <(printf file)",
            "cat < file",
            "cat << EOF",
            "cat file & echo done",
            "cat file |& head",
            "cat <<'EOF'\nbody\nEOF\nrm file",
        ] {
            assert_eq!(
                is_search_or_read_bash_command(command),
                BashSearchReadKind::default(),
                "command={command:?}"
            );
        }
        assert!(is_search_or_read_bash_command("cat <<'EOF'\nbody\nEOF").is_read);
        assert!(is_search_or_read_bash_command("cat \"$(printf file)\"").is_read);
    }

    #[test]
    fn bash_input_semantic_preprocessing_matches_official_schema() {
        let parsed = crate::tool::ToolCall::normalize_input(
            &BashTool,
            &serde_json::json!({
                "command": "echo ok",
                "timeout": "30",
                "run_in_background": "false",
                "dangerouslyDisableSandbox": "true"
            }),
        );
        assert_eq!(parsed["timeout"], serde_json::json!(30));
        assert_eq!(parsed["run_in_background"], serde_json::json!(false));
        assert_eq!(parsed["dangerouslyDisableSandbox"], serde_json::json!(true));
    }

    /// CC reads the background gate ONCE, at module load
    /// (`BashTool.tsx:332-334`, eslint-exempted with "Intentional: schema must
    /// be defined at module load"), then builds the schema from it inside
    /// `lazySchema`. A mid-process env flip does not re-shape the schema in
    /// either implementation, so this asserts the default shape and leaves the
    /// disabled shape to `bash_tool_schema_omits_background_when_gate_is_off`,
    /// which owns its own process under nextest.
    #[test]
    fn bash_tool_schema_matches_official_background_task_gate() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _disabled = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        let schema = bash_tool_schema();
        assert!(
            schema
                .input_schema
                .pointer("/properties/run_in_background")
                .is_some()
        );
        // Never model-facing: it is set by SedEditPermissionRequest after the
        // user approves a preview, and exposing it would pair an innocuous
        // command with an arbitrary file write.
        assert!(
            schema
                .input_schema
                .pointer("/properties/_simulatedSedEdit")
                .is_none()
        );
        assert!(
            schema
                .input_schema
                .pointer("/properties/dangerouslyDisableSandbox")
                .is_some()
        );
    }

    /// The other side of the gate. Separate test = separate process under
    /// nextest, which is what lets the one-shot read be observed at all.
    #[test]
    fn bash_tool_schema_omits_background_when_gate_is_off() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _enabled = EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "true");
        let schema = bash_tool_schema();
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

    #[test]
    fn bash_output_applies_approved_simulated_sed_edit_without_shelling_out() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let path = std::env::temp_dir().join(format!(
            "cometix-bash-sed-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, "hello old\n").unwrap();
        let args = serde_json::json!({
            "command": format!("sed -i 's/old/new/' {}", path.display()),
            "_simulatedSedEdit": {
                "filePath": path.display().to_string(),
                "newContent": "hello new\n",
            }
        });

        let context = crate::tool::ToolUseContext::default();
        let output = futures::executor::block_on(apply_simulated_sed_edit(
            &simulated_sed_edit_from_args(&args).unwrap(),
            args["command"].as_str().unwrap(),
            &context,
            None,
        ))
        .expect("simulated sed edit succeeds");
        let updated = std::fs::read_to_string(&path).unwrap();
        let read_state = context
            .read_file_state
            .get(&path)
            .expect("simulated sed refreshes Read state in its owner");
        let _ = std::fs::remove_file(&path);

        assert_eq!(updated, "hello new\n");
        assert_eq!(read_state.content.as_deref(), Some("hello new\n"));
        assert!(read_state.timestamp_ms.is_some());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(!output.interrupted);
    }

    #[test]
    fn bash_output_reports_missing_file_for_simulated_sed_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let path = std::env::temp_dir().join(format!(
            "cometix-missing-sed-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));
        let args = serde_json::json!({
            "command": format!("sed -i 's/old/new/' {}", path.display()),
            "_simulatedSedEdit": {
                "filePath": path.display().to_string(),
                "newContent": "hello new\n",
            }
        });

        let context = crate::tool::ToolUseContext::default();
        let output = futures::executor::block_on(apply_simulated_sed_edit(
            &simulated_sed_edit_from_args(&args).unwrap(),
            args["command"].as_str().unwrap(),
            &context,
            None,
        ))
        .expect("missing simulated sed file returns sed-style stderr");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.contains("No such file or directory"));
        assert!(!output.interrupted);
    }

    #[test]
    fn simulated_sed_respects_explicit_no_write_seam() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let context = crate::tool::ToolUseContext::default();
        let error = futures::executor::block_on(apply_simulated_sed_edit(
            &SimulatedSedEdit {
                file_path: "/tmp/cometix-must-not-write".to_string(),
                new_content: "blocked".to_string(),
            },
            "sed -i ...",
            &context,
            None,
        ))
        .unwrap_err();
        assert_eq!(
            error,
            crate::tools::shared::write_gate::SED_EDIT_DISABLED_ERROR
        );
    }

    #[test]
    fn map_uses_persisted_output_wrapper() {
        let big = "x".repeat(200);
        let path = std::env::temp_dir().join(format!(
            "cometix-bash-persisted-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, &big).unwrap();
        let path = path.display().to_string();
        let size = Some(big.len() as u64);

        let output = BashOutput {
            stdout: big[..64].to_string(),
            stderr: String::new(),
            interrupted: false,
            is_image: false,
            structured_content: None,
            background_task_id: None,
            persisted_output_path: Some(path.clone()),
            persisted_output_size: size,
            exit_code: Some(0),
            return_code_interpretation: None,
            cwd_after: None,
            command: "cat big".to_string(),
            ..Default::default()
        };
        let (mapped, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &BashTool,
            &crate::tool::ToolOutput::Bash(output),
            "toolu_bash",
        );
        assert!(matches!(
            status,
            crate::types::message::ToolResultStatus::Success
        ));
        assert!(mapped.starts_with(crate::utils::tool_result_storage::PERSISTED_OUTPUT_TAG));
        assert!(mapped.contains(&path));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn persisted_output_copies_verified_source_and_commits_atomically() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-persist-copy-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let source = root.join("source.output");
        std::fs::write(&source, b"0123456789").unwrap();
        let task_id = format!("task-{}", uuid::Uuid::new_v4().simple());
        let destination = persist_output_file(&source.display().to_string(), &task_id, 10)
            .expect("verified output is persisted");
        assert_eq!(std::fs::read(&source).unwrap(), b"0123456789");
        assert_eq!(std::fs::read(&destination).unwrap(), b"0123456789");
        assert!(
            !std::fs::read_dir(std::path::Path::new(&destination).parent().unwrap())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn persisted_output_rename_failure_keeps_source_artifact_and_cleans_temporary() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-persist-rename-failure-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let source = root.join("source.output");
        std::fs::write(&source, b"retained-output").unwrap();
        let task_id = format!("task-{}", uuid::Uuid::new_v4().simple());
        let destination = crate::utils::tool_result_storage::get_tool_result_path(&task_id, false);
        std::fs::create_dir_all(&destination).unwrap();

        assert!(
            persist_output_file(
                &source.display().to_string(),
                &task_id,
                b"retained-output".len() as u64,
            )
            .is_none()
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"retained-output");
        let parent = destination.parent().unwrap();
        assert!(
            !std::fs::read_dir(parent)
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn persisted_output_rejects_symlink_swap_without_truncating_target() {
        use std::os::unix::fs::symlink;

        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-persist-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let victim = root.join("victim");
        let source = root.join("swapped.output");
        std::fs::write(&victim, b"victim-must-remain-intact").unwrap();
        symlink(&victim, &source).unwrap();
        let task_id = format!("task-{}", uuid::Uuid::new_v4().simple());
        assert!(
            persist_output_file(&source.display().to_string(), &task_id, 65 * 1024 * 1024)
                .is_none()
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"victim-must-remain-intact"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_result_maps_abort_tag_without_synthetic_exit_code() {
        let output = BashOutput {
            stdout: "partial".to_string(),
            interrupted: true,
            exit_code: Some(137),
            command: "sleep 30".to_string(),
            ..Default::default()
        };
        let (mapped, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &BashTool,
            &crate::tool::ToolOutput::Bash(output),
            "toolu_interrupt",
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Error);
        assert_eq!(
            mapped,
            "partial\n<error>Command was aborted before completion</error>"
        );
        assert!(!mapped.contains("Exit code 137"));
    }

    #[test]
    fn foreground_timeout_maps_to_exit_143_without_abort_or_display_stderr() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _background = EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");
        let output = bash_output(
            &serde_json::json!({"command": "sleep 1", "timeout": 40}),
            &crate::tool::ToolUseContext::default(),
            None,
            None,
        )
        .expect("timeout is represented as a shell result");
        assert_eq!(output.exit_code, Some(143));
        assert!(!output.interrupted);
        assert_eq!(output.stdout, "Exit code 143");
        assert!(output.stderr.is_empty());
        let (mapped, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &BashTool,
            &crate::tool::ToolOutput::Bash(output.clone()),
            "toolu_timeout",
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Error);
        assert_eq!(mapped, "Exit code 143");
        assert!(!mapped.contains("aborted"));
        // The raw `toolUseResult` is the render source; the synthetic
        // timeout stderr stays off the wire.
        let raw = crate::tool::ToolCall::tool_use_result(
            &BashTool,
            &crate::tool::ToolOutput::Bash(output),
        )
        .expect("bash raw rides");
        assert_eq!(raw["stderr"], serde_json::json!(""));
    }

    #[test]
    fn file_mode_merges_stdout_stderr_and_preserves_partial_binary_tail() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-file-mode-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _background = EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");
        let output = bash_output(
            &serde_json::json!({
                "command": "printf a; printf b >&2; printf '\\377tail'"
            }),
            &crate::tool::ToolUseContext::default(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(output.stdout, "ab�tail");
        assert!(output.stderr.is_empty());
        assert_eq!(output.exit_code, Some(0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn no_write_pipe_fallback_keeps_child_stderr_model_visible() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-no-write-stderr-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let _background = EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");
        let output = bash_output(
            &serde_json::json!({"command": "printf visible-error >&2; exit 2"}),
            &crate::tool::ToolUseContext::default(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(output.stdout, "visible-error\nExit code 2");
        assert!(output.stderr.is_empty());
        let (mapped, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &BashTool,
            &crate::tool::ToolOutput::Bash(output),
            "toolu_no_write_stderr",
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Error);
        assert_eq!(mapped, "Exit code 2\nvisible-error");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_returns_on_shell_exit_without_waiting_for_grandchild_fd() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-grandchild-fd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _background = EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");
        // Snapshot creation is a one-time provider cost and is outside the
        // ShellCommand `exit` versus inherited-fd contract measured here.
        crate::utils::shell::get_shell_config().unwrap();
        let started = std::time::Instant::now();
        let output = bash_output(
            &serde_json::json!({
                "command": "(sleep 0.8; printf late) & printf early"
            }),
            &crate::tool::ToolUseContext::default(),
            None,
            None,
        )
        .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        assert_eq!(output.stdout, "early");
        assert_eq!(output.exit_code, Some(0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn foreground_timeout_moves_command_to_canonical_background_task() {
        struct TaskStateGuard;
        impl Drop for TaskStateGuard {
            fn drop(&mut self) {
                crate::tasks::local_shell_task::kill_shell_tasks::kill_all_shell_tasks();
                crate::tasks::local_shell_task::clear_for_test();
            }
        }

        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _tasks = TaskStateGuard;
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _background = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        crate::tasks::local_shell_task::clear_for_test();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
        let _ = crate::utils::shell::get_shell_config().expect("shell provider warms up");
        let started = std::time::Instant::now();
        let output = bash_output(
            &serde_json::json!({
                "command": "sleep 2.5; printf timeout-background-done",
                "timeout": 20
            }),
            &context,
            Some("toolu_timeout_background"),
            None,
        )
        .expect("timed-out foreground command is backgrounded");
        assert!(started.elapsed() < std::time::Duration::from_millis(2_400));
        let task_id = output
            .background_task_id
            .expect("timeout returns a background task ID");
        assert!(matches!(
            store.get().tasks.get(&task_id).map(|task| task.as_ref()),
            Some(crate::state::app_state_store::TaskState::LocalShell(task)) if task.is_backgrounded
        ));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline
            && store.get().tasks.get(&task_id).is_some_and(|task| {
                matches!(task.as_ref(), crate::state::app_state_store::TaskState::LocalShell(task) if task.status == "running")
            })
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let snapshot = crate::tasks::local_shell_task::task_output_snapshot(&task_id, Some(&store))
            .expect("background task remains in AppState");
        assert_eq!(snapshot.status, "completed");
        assert!(snapshot.output.contains("timeout-background-done"));

        let _ = std::fs::remove_file(crate::utils::task::disk_output::get_task_output_path(
            &task_id,
        ));
        crate::utils::message_queue_manager::dequeue_all_matching(|queued| {
            queued.value.contains(&task_id)
        });
        crate::tasks::local_shell_task::clear_for_test();
    }

    #[test]
    fn explicit_background_task_is_stoppable_through_canonical_task_state() {
        struct TaskStateGuard;
        impl Drop for TaskStateGuard {
            fn drop(&mut self) {
                crate::tasks::local_shell_task::kill_shell_tasks::kill_all_shell_tasks();
                crate::tasks::local_shell_task::clear_for_test();
            }
        }

        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _tasks = TaskStateGuard;
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _background = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS");
        crate::tasks::local_shell_task::clear_for_test();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
        let output = bash_output(
            &serde_json::json!({
                "command": "sleep 30",
                "run_in_background": true
            }),
            &context,
            Some("toolu_explicit_background"),
            None,
        )
        .expect("explicit background command starts");
        let task_id = output.background_task_id.expect("background task ID");
        assert!(matches!(
            store.get().tasks.get(&task_id).map(|task| task.as_ref()),
            Some(crate::state::app_state_store::TaskState::LocalShell(task))
                if task.status == "running" && task.is_backgrounded
        ));

        let stopped = futures::executor::block_on(crate::tools::task_stop_tool::task_stop_output(
            &serde_json::json!({"task_id": task_id}),
            Some(&store),
        ))
        .expect("TaskStop stops explicit Bash background task");
        assert_eq!(stopped.task_id, task_id);
        assert_eq!(stopped.task_type, "local_bash");
        assert_eq!(stopped.command.as_deref(), Some("sleep 30"));
        assert!(matches!(
            store.get().tasks.get(&task_id).map(|task| task.as_ref()),
            Some(crate::state::app_state_store::TaskState::LocalShell(task))
                if task.status == "killed" && task.notified
        ));
        assert!(crate::utils::task::disk_output::get_task_output_path(&task_id).exists());
        let _ = std::fs::remove_file(crate::utils::task::disk_output::get_task_output_path(
            &task_id,
        ));
        crate::utils::message_queue_manager::dequeue_all_matching(|queued| {
            queued.value.contains(&task_id)
        });
    }

    #[test]
    fn large_foreground_output_is_copied_to_retained_tool_result_artifact() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _max_output = EnvVarGuard::unset("BASH_MAX_OUTPUT_LENGTH");
        let output = bash_output(
            &serde_json::json!({
                "command": "printf '%040000d' 0 | tr '0' x"
            }),
            &crate::tool::ToolUseContext::default(),
            None,
            None,
        )
        .expect("large shell output succeeds");

        let persisted = output
            .persisted_output_path
            .as_ref()
            .map(std::path::PathBuf::from)
            .expect("large output is persisted");
        assert_eq!(output.persisted_output_size, Some(40_000));
        assert_eq!(std::fs::metadata(&persisted).unwrap().len(), 40_000);
        assert_eq!(output.stdout.len(), utils::BASH_MAX_OUTPUT_DEFAULT);

        let task_id = persisted.file_stem().unwrap().to_string_lossy();
        let raw = crate::utils::task::disk_output::get_task_output_path(&task_id);
        let _ = std::fs::remove_file(persisted);
        let _ = std::fs::remove_file(raw);
    }

    #[test]
    fn large_error_output_keeps_task_artifact_without_success_persistence_link() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _max_output = EnvVarGuard::unset("BASH_MAX_OUTPUT_LENGTH");
        let directory = crate::utils::task::disk_output::get_task_output_dir();
        std::fs::create_dir_all(&directory).unwrap();
        let before = std::fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect::<std::collections::BTreeSet<_>>();
        let output = bash_output(
            &serde_json::json!({
                "command": "printf '%040000d' 0 | tr '0' e; exit 7"
            }),
            &crate::tool::ToolUseContext::default(),
            None,
            None,
        )
        .expect("large failing shell output remains a semantic result");
        assert_eq!(output.exit_code, Some(7));
        assert!(output.persisted_output_path.is_none());
        let mut created = std::fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| !before.contains(path))
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 1);
        let artifact = created.pop().unwrap();
        assert_eq!(std::fs::metadata(&artifact).unwrap().len(), 40_000);
        let _ = std::fs::remove_file(artifact);
    }

    #[test]
    fn bash_provider_sources_parent_session_environment_file() {
        struct CacheGuard;
        impl Drop for CacheGuard {
            fn drop(&mut self) {
                crate::utils::session_environment::invalidate_session_env_cache();
            }
        }

        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _cache = CacheGuard;
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let path = std::env::temp_dir().join(format!(
            "cometix-parent-session-env-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, "export COMETIX_SESSION_ENV_TEST=visible").unwrap();
        let _env_file = EnvVarGuard::set("CLAUDE_ENV_FILE", &path);
        crate::utils::session_environment::invalidate_session_env_cache();

        let output = bash_output(
            &serde_json::json!({"command": "printf %s \"$COMETIX_SESSION_ENV_TEST\""}),
            &crate::tool::ToolUseContext::default(),
            None,
            None,
        )
        .expect("session environment is sourced");
        let _ = std::fs::remove_file(path);
        assert_eq!(output.stdout, "visible");
    }

    #[test]
    fn bash_output_uses_cwd_override_like_official_run_with_cwd_override() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-bash-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let args = serde_json::json!({"command": "pwd -P"});

        let context = crate::tool::ToolUseContext::default()
            .with_cwd_override(Some(dir.clone()))
            .with_agent_id(Some("test-agent".to_string()));
        let output =
            bash_output(&args, &context, None, None).expect("bash command runs under cwd override");
        let expected = dir.canonicalize().unwrap().display().to_string();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(output.stdout.trim(), expected);
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn foreground_shell_reports_persistent_physical_cwd() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-bash-persistent-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let child = dir.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let mut permission = crate::tool::ToolPermissionContext::default();
        permission.additional_working_directories.insert(
            dir.display().to_string(),
            crate::types::permissions::AdditionalWorkingDirectory {
                path: dir.display().to_string(),
                source: crate::types::permissions::PermissionRuleSource::Session,
            },
        );
        let context = crate::tool::ToolUseContext::with_permission_context(permission)
            .with_cwd_override(Some(dir.clone()));
        let output = bash_output(
            &serde_json::json!({"command": "cd child"}),
            &context,
            None,
            None,
        )
        .expect("cd succeeds");
        assert_eq!(output.cwd_after, Some(child.canonicalize().unwrap()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nonzero_exit_uses_semantic_shell_error_status() {
        let context = crate::tool::ToolUseContext::default();
        let output = bash_output(
            &serde_json::json!({"command": "printf boom >&2; exit 7"}),
            &context,
            None,
            None,
        )
        .expect("shell process returns an output");
        assert_eq!(output.exit_code, Some(7));
        assert!(output.stdout.contains("Exit code 7"));
        let (_, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &BashTool,
            &crate::tool::ToolOutput::Bash(output),
            "toolu_error",
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Error);
    }

    #[test]
    fn map_is_image_returns_ui_label_and_render_clears_stdout() {
        let data_uri = "data:image/png;base64,iVBORw0KGgo=";
        let output = BashOutput {
            stdout: data_uri.to_string(),
            stderr: String::new(),
            interrupted: false,
            is_image: true,
            structured_content: None,
            background_task_id: None,
            persisted_output_path: None,
            persisted_output_size: None,
            exit_code: Some(0),
            return_code_interpretation: None,
            cwd_after: None,
            command: "cat img".to_string(),
            ..Default::default()
        };
        let (mapped, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &BashTool,
            &crate::tool::ToolOutput::Bash(output.clone()),
            "toolu_img",
        );
        assert!(matches!(
            status,
            crate::types::message::ToolResultStatus::Success
        ));
        assert_eq!(mapped, "[Image data detected and sent to Claude]");

        // The raw keeps the data URI (CC wire shape); the image branch
        // of the renderer must not dump it into the transcript.
        let lines = ui::render_tool_result_message(
            &output,
            None,
            crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
        );
        assert_eq!(lines.len(), 1, "UI must not dump the data URI");
        assert_eq!(lines[0].text, "[Image data detected and sent to Claude]");
    }

    #[test]
    fn map_structured_content_short_circuits_like_official() {
        let output = BashOutput {
            stdout: "ignored".to_string(),
            stderr: String::new(),
            interrupted: false,
            is_image: false,
            structured_content: Some(vec![serde_json::json!({
                "type": "text",
                "text": "from structured"
            })]),
            background_task_id: None,
            persisted_output_path: None,
            persisted_output_size: None,
            exit_code: Some(0),
            return_code_interpretation: None,
            cwd_after: None,
            command: "noop".to_string(),
            ..Default::default()
        };
        let (mapped, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &BashTool,
            &crate::tool::ToolOutput::Bash(output),
            "toolu_struct",
        );
        assert!(matches!(
            status,
            crate::types::message::ToolResultStatus::Success
        ));
        assert!(mapped.is_empty());
    }
}
