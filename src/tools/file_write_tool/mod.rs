//! FileWriteTool — create or overwrite a text file.
//!
//! Maps to: CC `tools/FileWriteTool/FileWriteTool.ts`.
//! File reads, writes, path expansion, permission policy, history, Git diff,
//! LSP, and VS Code notifications remain in their source-shaped owners.

pub mod prompt;
pub mod ui;

use crate::tool::{
    PermissionPatternMatcher, ToolCall, ToolOutput, ToolResult, ToolUseContext, ValidationResult,
};
use crate::types::message::{AssistantMessage, StructuredDiffHunk, ToolResultStatus};
use futures::FutureExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct FileWriteTool;

/// Private `PermissionRequest.call_input` transport. This value never enters
/// SDK messages or transcripts because `call_input` is `serde(skip)`.
pub(crate) const CHECKED_WRITE_DESTINATION_KEY: &str = "__cometix_checked_write_destination";

pub(crate) fn resolved_write_destination(path: &Path) -> PathBuf {
    let candidate = crate::utils::fs_operations::get_paths_for_permission_check(path)
        .into_iter()
        .last()
        .unwrap_or_else(|| path.to_path_buf());
    crate::utils::path::expand_path(&candidate.display().to_string(), path.parent())
        .unwrap_or(candidate)
}

fn expected_write_destination(
    request: &crate::types::permissions::PermissionRequest,
    context: &ToolUseContext,
) -> Option<PathBuf> {
    let call_input = request.call_input.as_ref()?;
    let original_path = call_input.get("file_path")?.as_str()?;
    let observable_original =
        crate::utils::path::expand_path(original_path, Some(&context.effective_cwd())).ok()?;
    let approved_path = request.input.get("file_path")?.as_str()?;
    if Path::new(approved_path) != observable_original {
        // An explicit hook/user path rewrite is authoritative by contract.
        return None;
    }
    call_input
        .get(CHECKED_WRITE_DESTINATION_KEY)?
        .as_str()
        .map(PathBuf::from)
}

fn is_non_regular_existing_path(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && !metadata.is_file())
}

/// Maps to: CC `FileWriteTool.ts:56-65` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "file_path",
                zod::string().describe(
                    "The absolute path to the file to write (must be absolute, not relative)",
                ),
            ),
            (
                "content",
                zod::string().describe("The content to write to the file"),
            ),
        ])
    })
}

/// Maps to CC FileWriteTool's strict zod `inputSchema`.
pub fn file_write_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::FILE_WRITE_TOOL_NAME.to_string(),
        description: prompt::get_write_tool_description(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        strict: Some(true),
        ..Default::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOutputKind {
    Create,
    Update,
}

#[derive(Clone, Debug)]
pub struct WriteOutput {
    pub(crate) kind: WriteOutputKind,
    pub(crate) file_path: String,
    pub(crate) content: String,
    pub(crate) structured_patch: Vec<StructuredDiffHunk>,
    pub(crate) original_file: Option<String>,
    /// Captured synchronously immediately after the write, before remote Git
    /// diff awaits, matching CC's `readFileState.set(...timestamp)` ordering.
    pub(crate) read_timestamp_ms: i64,
    pub(crate) git_diff: Option<crate::utils::git_diff::ToolUseDiff>,
    pub(crate) dynamic_skill_dirs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct WriteErrorOutput {
    pub(crate) content: String,
    pub(crate) dynamic_skill_dirs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileWriteInput {
    file_path: String,
    content: String,
}

impl FileWriteInput {
    /// Maps to CC's strict zod `inputSchema` data projection.
    fn from_args(args: &serde_json::Value) -> Result<Self, String> {
        let file_path = args
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "file_path must be a string".to_string())?;
        let content = args
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "content must be a string".to_string())?;
        Ok(Self {
            file_path: file_path.to_string(),
            content: content.to_string(),
        })
    }

    fn full_path(&self, context: &ToolUseContext) -> Result<PathBuf, String> {
        crate::utils::path::expand_path(&self.file_path, Some(&context.effective_cwd()))
    }
}

fn validation_error(message: impl Into<String>, error_code: i32) -> ValidationResult {
    ValidationResult::Error {
        message: message.into(),
        error_code,
    }
}

/// Maps to CC `FileWriteTool.validateInput(...)`.
fn validate_write_input(input: &FileWriteInput, context: &ToolUseContext) -> ValidationResult {
    let full_path = match input.full_path(context) {
        Ok(path) => path,
        Err(error) => return validation_error(error, 1),
    };

    if let Err(error) =
        crate::services::team_memory_sync::team_mem_secret_guard::check_team_mem_secrets(
            &full_path,
            &input.content,
        )
    {
        return validation_error(error, 0);
    }

    if crate::utils::permissions::filesystem::matching_rule_for_input(
        &full_path.display().to_string(),
        &context.tool_permission_context,
        crate::utils::permissions::filesystem::FilePermissionType::Edit,
        crate::types::permissions::PermissionBehavior::Deny,
        &context.effective_cwd(),
    )
    .is_some()
    {
        return validation_error(
            "File is in a directory that is denied by your permission settings.",
            1,
        );
    }

    let path_string = full_path.to_string_lossy();
    if path_string.starts_with("//") || path_string.starts_with("\\\\") {
        // CC deliberately avoids stat on UNC paths before the permission layer.
        return ValidationResult::Ok;
    }

    let metadata = match futures::executor::block_on(
        crate::utils::fs_operations::get_fs_implementation().stat(&full_path),
    ) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ValidationResult::Ok;
        }
        // CC rethrows a non-ENOENT stat failure to the outer tool-call error
        // boundary instead of returning a semantic error code.
        Err(error) => {
            return ValidationResult::Fatal {
                message: error.to_string(),
            };
        }
    };

    if !metadata.is_file() {
        // DEVIATION(SECURITY): CC writes through to the special file. Reject
        // before opening a FIFO or device path.
        return validation_error("Cannot write to a non-regular file.", 1);
    }

    let Some(read_state) = context.read_file_state.get(&full_path) else {
        return validation_error(
            "File has not been read yet. Read it first before writing to it.",
            2,
        );
    };
    if read_state.is_partial_view {
        return validation_error(
            "File has not been read yet. Read it first before writing to it.",
            2,
        );
    }

    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    if mtime > read_state.timestamp_ms.unwrap_or(0) {
        return validation_error(
            "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it.",
            3,
        );
    }

    ValidationResult::Ok
}

fn make_error(message: impl Into<String>, _start: Instant) -> ToolResult {
    ToolResult {
        data: ToolOutput::Composed {
            content: format!("<tool_use_error>{}</tool_use_error>", message.into()),
            status: ToolResultStatus::Error,
        },
        new_messages: Vec::new(),
    }
}

fn make_error_after_discovery(
    message: impl Into<String>,
    dynamic_skill_dirs: Vec<String>,
) -> ToolResult {
    ToolResult {
        data: ToolOutput::WriteError(WriteErrorOutput {
            content: format!("<tool_use_error>{}</tool_use_error>", message.into()),
            dynamic_skill_dirs,
        }),
        new_messages: Vec::new(),
    }
}

fn remote_git_diff_enabled() -> bool {
    // Maps to `getFeatureValue_CACHED_MAY_BE_STALE('tengu_quartz_lantern', false)`,
    // resolved from the source-controlled switch table instead of GrowthBook.
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::RemoteGitDiff,
    )
}

impl ToolCall for FileWriteTool {
    fn name(&self) -> &'static str {
        prompt::FILE_WRITE_TOOL_NAME
    }

    /// Maps to: CC `FileWriteTool.ts:108-110` `async prompt() { return
    /// getWriteToolDescription() }` — same source the wire schema renders.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_write_tool_description()
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("create or overwrite files")
    }

    /// Maps to: CC `FileWriteTool.ts:99-101` `description()` — the constant
    /// permission-dialog line; the input is not read.
    fn description(&self, _args: &serde_json::Value) -> String {
        "Write a file to the local filesystem.".to_string()
    }

    /// Maps to: CC `FileWriteTool.ts:103` mounting `UI.tsx#getToolUseSummary`.
    /// Maps to: CC `FileWriteTool.ts:102` mounting
    /// `UI.tsx:86-93#userFacingName` (`Updated plan` for plan paths, else
    /// `Write`).
    fn user_facing_name(&self, args: Option<&serde_json::Value>) -> String {
        crate::tools::file_write_tool::ui::user_facing_name(args)
    }

    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        crate::tools::file_write_tool::ui::get_tool_use_summary(Some(args))
    }

    /// Maps to: CC `FileWriteTool.ts:104-107` `getActivityDescription(input)`.
    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            crate::tools::file_write_tool::ui::get_tool_use_summary(Some(args))
                .map(|summary| format!("Writing {summary}"))
                .unwrap_or_else(|| "Writing file".to_string()),
        )
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `utils/api.ts:646-660` `normalizeToolInput` FileWriteTool
    /// branch: markdown keeps trailing whitespace (two trailing spaces are a
    /// hard line break — api.ts:650), everything else is stripped. The
    /// `/\.(md|mdx)$/i` test is the case-insensitive suffix check below.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let Ok(input) = FileWriteInput::from_args(args) else {
            return args.clone();
        };
        let lower = input.file_path.to_ascii_lowercase();
        if lower.ends_with(".md") || lower.ends_with(".mdx") {
            return args.clone();
        }
        let mut parsed = args.clone();
        if let Some(object) = parsed.as_object_mut() {
            object.insert(
                "content".to_string(),
                serde_json::Value::String(
                    crate::tools::file_edit_tool::utils::strip_trailing_whitespace(&input.content),
                ),
            );
        }
        parsed
    }

    /// Maps to: CC `FileWriteTool.ts:146-152` `extractSearchText() => ''`:
    /// transcript render shows either content (create) or a structured diff
    /// (update); the heuristic's 'content' allowlist key would index the raw
    /// content string even in update mode where it is NOT shown — phantom.
    /// `Some("")` is the explicit opt-out (tool.rs:2152-2157).
    fn extract_search_text(&self, _data: &ToolOutput) -> Option<String> {
        Some(String::new())
    }

    /// Maps to: CC `FileWriteTool.ts:112` mounting `UI.tsx:99-109`
    /// `isResultTruncated`.
    fn is_result_truncated(&self, data: &ToolOutput) -> bool {
        match data {
            ToolOutput::Write(output) => ui::is_result_truncated(output),
            _ => false,
        }
    }

    fn backfill_observable_input(
        &self,
        args: &serde_json::Value,
        context: &ToolUseContext,
    ) -> serde_json::Value {
        let Ok(input) = FileWriteInput::from_args(args) else {
            return args.clone();
        };
        let Ok(full_path) = input.full_path(context) else {
            return args.clone();
        };
        let mut observable = args.clone();
        if let Some(object) = observable.as_object_mut() {
            object.insert(
                "file_path".to_string(),
                serde_json::Value::String(full_path.display().to_string()),
            );
        }
        observable
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &ToolUseContext,
    ) -> ValidationResult {
        match FileWriteInput::from_args(args) {
            Ok(input) => validate_write_input(&input, context),
            Err(error) => validation_error(error, 1),
        }
    }

    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let path = FileWriteInput::from_args(args)
            .and_then(|input| input.full_path(context))
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| {
                args.get("file_path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            });
        crate::utils::permissions::filesystem::check_write_permission_for_tool(
            &path,
            args,
            &context.tool_permission_context,
            &context.effective_cwd(),
        )
    }

    fn get_path(&self, args: &serde_json::Value) -> Option<String> {
        FileWriteInput::from_args(args)
            .ok()
            .map(|input| input.file_path)
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        FileWriteInput::from_args(args)
            .map(|input| format!("{}: {}", input.file_path, input.content))
            .unwrap_or_default()
    }

    fn prepare_permission_matcher(
        &self,
        args: &serde_json::Value,
    ) -> Option<PermissionPatternMatcher> {
        let path = self.get_path(args)?;
        Some(Box::new(move |pattern| {
            crate::utils::permissions::shell_rule_matching::match_wildcard_pattern(
                pattern, &path, false,
            )
        }))
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        parent_message: Option<&'a AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, ToolResult> {
        async move {
            let start = Instant::now();
            let input = match FileWriteInput::from_args(args) {
                Ok(input) => input,
                Err(error) => return make_error(error, start),
            };
            let full_path = match input.full_path(context) {
                Ok(path) => path,
                Err(error) => return make_error(error, start),
            };

            // DEVIATION(SECURITY): Cometix's repository-wide no-write mode
            // has no CC equivalent. It must fail before discovery, mkdir,
            // history, temporary files, or any other disk mutation.
            if !crate::utils::env_utils::is_cometix_write_enabled() {
                return make_error(
                    crate::tools::shared::write_gate::FILE_WRITE_DISABLED_ERROR,
                    start,
                );
            }

            let touched_paths = [full_path.clone()];
            let cwd = context.effective_cwd();
            let skill_dirs = crate::skills::load_skills_dir::discover_skill_dirs_for_paths(
                &touched_paths,
                &cwd,
            );
            let dynamic_skill_dirs = skill_dirs
                .iter()
                .map(|directory| directory.display().to_string())
                .collect::<Vec<_>>();
            if let Some(triggers) = &context.dynamic_skill_dir_triggers {
                triggers.extend(dynamic_skill_dirs.iter().cloned());
            }
            // DEVIATION(L1): CC starts `addSkillDirectories(...).catch(...)`
            // without awaiting it. The Rust loader is synchronous, so running
            // it inline avoids an untracked background thread while preserving
            // the same loaded skill set before the next turn.
            crate::skills::load_skills_dir::add_skill_directories(&skill_dirs);
            crate::skills::load_skills_dir::activate_conditional_skills_for_paths(
                &touched_paths,
                &cwd,
            );

            crate::services::diagnostic_tracking::before_file_edited(
                &full_path.display().to_string(),
            )
            .await;

            // SECURITY HARDENING: pin the permission-checked physical
            // destination after all pre-write awaits. Parent/direct symlink
            // swaps cannot redirect mkdir, history, freshness, or atomic
            // replacement to a different path after approval.
            let write_target = resolved_write_destination(&full_path);
            if expected_write_destination(request, context)
                .is_some_and(|expected| expected != write_target)
            {
                return make_error_after_discovery(
                    "File has been unexpectedly modified. Read it again before attempting to write it.",
                    dynamic_skill_dirs,
                );
            }
            // DEVIATION(SECURITY): CC writes through to the special file.
            if is_non_regular_existing_path(&write_target) {
                return make_error_after_discovery(
                    "Cannot write to a non-regular file.",
                    dynamic_skill_dirs,
                );
            }
            if let Err(error) = crate::utils::fs_operations::get_fs_implementation().mkdir(
                write_target
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")), None,
            ).await {
                return make_error_after_discovery(error.to_string(), dynamic_skill_dirs);
            }

            if crate::utils::file_history::file_history_enabled() {
                if let Some(store) = context.app_store.store.as_ref() {
                    crate::utils::file_history::file_history_track_edit(
                        store,
                        &full_path.display().to_string(),
                        // The ENVELOPE uuid — CC passes `parentMessage.uuid`
                        // (`FileWriteTool.ts:261`), which is what
                        // `fileHistoryRewind` matches on (`fileHistory.ts:367`).
                        parent_message
                            .map(|message| message.uuid.as_str())
                            .unwrap_or_default(),
                    )
                    .await;
                }
            }

            // File history is the final await before the atomic read/write
            // section. Re-resolve once more so a swap during backup fails
            // closed; there are no async yields after this point.
            let critical_write_target = resolved_write_destination(&full_path);
            if critical_write_target != write_target {
                return make_error_after_discovery(
                    "File has been unexpectedly modified. Read it again before attempting to write it.",
                    dynamic_skill_dirs,
                );
            }
            let write_target = critical_write_target;

            let (old_content, encoding) =
                match crate::utils::file_read::read_file_sync_with_metadata(&write_target) {
                    Ok(metadata) => (Some(metadata.content), metadata.encoding),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        (None, crate::utils::file_read::FileEncoding::Utf8)
                    }
                    Err(error) => {
                        return make_error_after_discovery(
                            error.to_string(),
                            dynamic_skill_dirs,
                        );
                    }
                };

            if old_content.is_some() {
                // Maps to: CC `FileWriteTool.ts:280` `getFileModificationTime`
                // (file.ts:66-69) — statSync failure throws into the tool
                // error channel; it does not silently pass staleness.
                let current_mtime =
                    match crate::utils::file::get_file_modification_time_result(&write_target) {
                        Ok(timestamp) => timestamp,
                        Err(error) => {
                            return make_error_after_discovery(
                                error.to_string(),
                                dynamic_skill_dirs,
                            );
                        }
                    };
                let Some(original_read_state) = context.read_file_state.get(&full_path) else {
                    return make_error_after_discovery(
                        "File has been unexpectedly modified. Read it again before attempting to write it.",
                        dynamic_skill_dirs,
                    );
                };
                if current_mtime > original_read_state.timestamp_ms.unwrap_or(0) {
                    let is_full_read =
                        original_read_state.offset.is_none() && original_read_state.limit.is_none();
                    if !is_full_read
                        || original_read_state.content.as_deref() != old_content.as_deref()
                    {
                        return make_error_after_discovery(
                            "File has been unexpectedly modified. Read it again before attempting to write it.",
                            dynamic_skill_dirs,
                        );
                    }
                }
            }

            if let Err(error) = crate::utils::file::write_text_content_to_target(
                &write_target,
                &input.content,
                encoding,
                crate::utils::file_read::LineEndingType::Lf,
            ) {
                return make_error_after_discovery(error.to_string(), dynamic_skill_dirs);
            }

            // Maps to: CC `FileWriteTool.ts:308-326` — the whole LSP block,
            // including the diagnostics clear, is gated on the manager
            // existing. DEVIATION(L1), verdict E5: CC issues didChange/didSave
            // as two fire-and-forget promises, but that concurrency is only
            // "issue both notifications in order without awaiting responses"
            // under JS single-threading, so sequential awaits of the pair are
            // the faithful projection.
            //
            // Scope correction (2026-08-29, T4 LSP audit H1): the empty-slot
            // hazard this comment used to cite was never an Edit/Write-layer
            // property — it was the LSP manager's ownership model. That is
            // fixed at its own layer: `services/lsp/manager.rs` now shares one
            // `Arc<LspServerManager>` the way CC shares one
            // `lspManagerInstance` (`manager.ts:20`), so nothing here can
            // observe an empty slot. The E5 ruling above stands unchanged and
            // is now about the pair's ordering only.
            if crate::services::lsp::manager::has_lsp_server_manager() {
                // Clear previously delivered diagnostics so new ones will be
                // shown.
                crate::services::lsp::diagnostic_registry::clear_delivered_diagnostics_for_file(
                    &format!("file://{}", full_path.display()),
                );
                let lsp_path = full_path.display().to_string();
                let lsp_content = input.content.clone();
                tokio::spawn(async move {
                    // didChange: content has been modified.
                    if let Err(error) =
                        crate::services::lsp::manager::change_file(&lsp_path, lsp_content).await
                    {
                        crate::utils::debug::log_for_debugging(&format!(
                            "LSP: Failed to notify server of file change for {lsp_path}: {error}"
                        ));
                        // CC: logError(err) — joins with the error log.
                    }
                    // didSave: file has been saved to disk (triggers
                    // diagnostics in the TypeScript server).
                    if let Err(error) = crate::services::lsp::manager::save_file(&lsp_path).await
                    {
                        crate::utils::debug::log_for_debugging(&format!(
                            "LSP: Failed to notify server of file save for {lsp_path}: {error}"
                        ));
                        // CC: logError(err) — joins with the error log.
                    }
                });
            }

            // Maps to: CC `FileWriteTool.ts:329` notifyVscodeFileUpdated.
            let vscode_path = full_path.display().to_string();
            let vscode_old_content = old_content.clone();
            let vscode_new_content = input.content.clone();
            tokio::spawn(async move {
                let _ = crate::services::mcp::vscode_sdk_mcp::notify_vscode_file_updated(
                    &vscode_path,
                    vscode_old_content.as_deref(),
                    Some(&vscode_new_content),
                )
                .await;
            });

            let read_timestamp_ms =
                match crate::utils::file::get_file_modification_time_result(&write_target) {
                    Ok(timestamp) => timestamp,
                    Err(error) => {
                        return make_error_after_discovery(
                            error.to_string(),
                            dynamic_skill_dirs,
                        );
                    }
                };
            // Maps to: CC `FileWriteTool.ts:332-338` source-position
            // `readFileState.set(fullFilePath, ...)`.
            context.read_file_state.set_entry(
                crate::utils::query_helpers::ReadFileStateEntry {
                    path: full_path.display().to_string(),
                    content: Some(input.content.clone()),
                    timestamp_ms: Some(read_timestamp_ms),
                    offset: None,
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Write,
                },
            );

            // CC: FileWriteTool.ts:339-342 `logEvent('tengu_write_claudemd',
            // {})` on `${sep}CLAUDE.md` paths — joins with analytics.

            let git_diff = if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref())
                && remote_git_diff_enabled()
            {
                // CC: logEvent('tengu_tool_use_diff_computed', {isWriteTool,
                // durationMs, hasDiff}) after the fetch (FileWriteTool.ts:
                // 349-356) — joins with analytics.
                crate::utils::git_diff::fetch_single_file_git_diff(&full_path).await
            } else {
                None
            };

            let (kind, structured_patch, original_file) =
                if old_content.as_ref().is_some_and(|content| !content.is_empty()) {
                    let old_content = old_content.unwrap_or_default();
                    let patch = crate::utils::diff::get_patch_for_display(
                        &old_content,
                        &[crate::utils::diff::DisplayEdit {
                            old_string: &old_content,
                            new_string: &input.content,
                            replace_all: false,
                        }],
                    );
                    // Maps to: CC `FileWriteTool.ts:381` `countLinesChanged(patch)`.
                    crate::utils::diff::count_lines_changed(&patch, None);
                    // CC: logFileOperation({operation: 'write', tool:
                    // 'FileWriteTool', filePath, type: 'update'})
                    // (FileWriteTool.ts:383-388) — joins with analytics
                    // (utils/fileOperationAnalytics.ts).
                    (WriteOutputKind::Update, patch, Some(old_content))
                } else {
                    // Maps to: CC `FileWriteTool.ts:404-405`
                    // `countLinesChanged([], content)` — the new-file branch
                    // counts every content line as an addition.
                    crate::utils::diff::count_lines_changed(&[], Some(&input.content));
                    // CC: logFileOperation({operation: 'write', tool:
                    // 'FileWriteTool', filePath, type: 'create'})
                    // (FileWriteTool.ts:407-412) — joins with analytics
                    // (utils/fileOperationAnalytics.ts).
                    (WriteOutputKind::Create, Vec::new(), None)
                };

            ToolResult {
                data: ToolOutput::Write(WriteOutput {
                    kind,
                    file_path: input.file_path,
                    content: input.content,
                    structured_patch,
                    original_file,
                    read_timestamp_ms,
                    git_diff,
                    dynamic_skill_dirs,
                }),
                new_messages: Vec::new(),
            }
        }
        .boxed()
    }

    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &ToolOutput,
        _tool_use_id: &str,
    ) -> (String, ToolResultStatus) {
        match data {
            ToolOutput::Write(output) => match output.kind {
                WriteOutputKind::Create => (
                    format!("File created successfully at: {}", output.file_path),
                    ToolResultStatus::Success,
                ),
                WriteOutputKind::Update => (
                    format!(
                        "The file {} has been updated successfully.",
                        output.file_path
                    ),
                    ToolResultStatus::Success,
                ),
            },
            ToolOutput::WriteError(output) => (output.content.clone(), ToolResultStatus::Error),
            ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>Invalid Write output</tool_use_error>".to_string(),
                ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording FileWriteTool's `Output` as the message's
    /// `toolUseResult` — the render layer parses it back with
    /// `ui::parse_output`. A failure records the unwrapped `Error: {message}`
    /// string instead (CC `toolExecution.ts:1726`).
    fn tool_use_result(&self, data: &ToolOutput) -> Option<serde_json::Value> {
        match data {
            ToolOutput::Write(output) => Some(ui::output_to_value(output)),
            ToolOutput::WriteError(output) => {
                let error = crate::utils::messages::extract_tag(&output.content, "tool_use_error")
                    .unwrap_or_else(|| output.content.clone());
                Some(serde_json::Value::String(format!("Error: {error}")))
            }
            ToolOutput::Composed {
                content,
                status: ToolResultStatus::Error,
            } => {
                let error = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(format!("Error: {error}")))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionMode, PermissionRequest, PermissionRuleValue};
    use crate::utils::query_helpers::{ReadFileStateEntry, ReadFileStateSource};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct TestGlobalConfigRestore;

    impl Drop for TestGlobalConfigRestore {
        fn drop(&mut self) {
            crate::utils::config::set_test_global_config(None);
        }
    }

    struct BootstrapRestore {
        session_id: String,
        original_cwd: PathBuf,
        interactive: bool,
        session_persistence_disabled: bool,
    }

    impl BootstrapRestore {
        fn capture() -> Self {
            Self {
                session_id: crate::bootstrap::state::get_session_id(),
                original_cwd: crate::bootstrap::state::get_original_cwd(),
                interactive: crate::bootstrap::state::get_is_interactive(),
                session_persistence_disabled:
                    crate::bootstrap::state::is_session_persistence_disabled(),
            }
        }
    }

    impl Drop for BootstrapRestore {
        fn drop(&mut self) {
            crate::bootstrap::state::set_session_id(&self.session_id);
            crate::bootstrap::state::set_original_cwd(&self.original_cwd);
            crate::bootstrap::state::set_is_interactive(self.interactive);
            crate::bootstrap::state::set_session_persistence_disabled(
                self.session_persistence_disabled,
            );
            crate::utils::session_storage::reset_session_file_pointer();
        }
    }

    fn request(path: &Path, content: &str) -> PermissionRequest {
        PermissionRequest {
            permission_result: None,
            id: "perm-write".to_string(),
            tool_use_id: "toolu-write".to_string(),
            tool_name: "Write".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: path.display().to_string(),
            input: serde_json::json!({
                "file_path": path.display().to_string(),
                "content": content,
            }),
            call_input: None,
            rule: PermissionRuleValue::new("Write", Some(path.display().to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::AcceptEdits,
        }
    }

    fn read_state(path: &Path, content: &str) -> ReadFileStateEntry {
        ReadFileStateEntry {
            path: path.display().to_string(),
            content: Some(content.to_string()),
            timestamp_ms: crate::utils::file::get_file_modification_time(path),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: ReadFileStateSource::Read,
        }
    }

    #[cfg(unix)]
    #[test]
    fn validation_rejects_fifo_without_opening_or_blocking_on_it() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = std::env::temp_dir().join(format!(
            "cometix-write-fifo-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("pipe");
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        let args = serde_json::json!({
            "file_path": path.display().to_string(),
            "content": "not written"
        });
        let mut context = ToolUseContext::default();
        context.read_file_state.set_entry(read_state(&path, ""));
        assert_eq!(
            FileWriteTool.validate_input(&args, &context),
            ValidationResult::Error {
                message: "Cannot write to a non-regular file.".to_string(),
                error_code: 1,
            }
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn creates_file_and_parent_directories_through_shared_writer() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-create-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let path = root.join("nested/new.txt");
        let request = request(&path, "hello\n");
        let result = FileWriteTool
            .call(
                &request.input,
                &request,
                &ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;
        let ToolOutput::Write(output) = result.data else {
            panic!("expected Write output")
        };
        assert_eq!(output.kind, WriteOutputKind::Create);
        assert_eq!(
            Some(output.read_timestamp_ms),
            crate::utils::file::get_file_modification_time(&path)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_call_preserves_dangling_direct_symlink_and_creates_its_target() {
        use std::os::unix::fs::symlink;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-call-dangling-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.txt");
        let link = root.join("link.txt");
        symlink("target.txt", &link).unwrap();
        let request = request(&link, "created");
        let result = FileWriteTool
            .call(
                &request.input,
                &request,
                &ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(result.data, ToolOutput::Write(_)));
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "created");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn existing_file_requires_full_read_and_rejects_content_race() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-race-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("existing.txt");
        std::fs::write(&path, "old\n").unwrap();

        let input = serde_json::json!({"file_path": path, "content": "new\n"});
        assert!(matches!(
            FileWriteTool.validate_input(&input, &ToolUseContext::default()),
            ValidationResult::Error { error_code: 2, .. }
        ));

        let mut context = ToolUseContext::default();
        context
            .read_file_state
            .set_entry(read_state(&path, "old\n"));
        let decoy = root.join("decoy.txt");
        context
            .read_file_state
            .set_entry(read_state(&decoy, "decoy\n"));
        assert_eq!(
            FileWriteTool.validate_input(&input, &context),
            ValidationResult::Ok
        );
        assert_eq!(
            context.read_file_state.keys(),
            vec![path.display().to_string(), decoy.display().to_string()]
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, "raced\n").unwrap();
        let request = request(&path, "new\n");
        let result = FileWriteTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let ToolOutput::WriteError(output) = result.data else {
            panic!("race must fail")
        };
        assert!(output.content.contains("unexpectedly modified"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "raced\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn post_validation_mtime_change_with_identical_content_uses_official_fallback() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-mtime-fallback-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("existing.txt");
        std::fs::write(&path, "same\n").unwrap();
        let mut context = ToolUseContext::default();
        context
            .read_file_state
            .set_entry(read_state(&path, "same\n"));
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, "same\n").unwrap();

        let request = request(&path, "updated\n");
        let result = FileWriteTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        assert!(matches!(result.data, ToolOutput::Write(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn preserves_utf16_encoding_but_honors_explicit_lf_content() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-encoding-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("utf16.txt");
        let mut bytes = vec![0xff, 0xfe];
        for unit in "old\r\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();
        // ReadTool decodes through UTF-8 with replacement; WriteTool detects
        // UTF-16 independently. With unchanged mtime, CC permits the rewrite.
        let old = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        let mut context = ToolUseContext::default();
        context.read_file_state.set_entry(read_state(&path, &old));
        let request = request(&path, "new\n");
        let result = FileWriteTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        assert!(matches!(result.data, ToolOutput::Write(_)));
        let bytes = std::fs::read(&path).unwrap();
        assert_ne!(bytes.get(..2), Some(&[0xff, 0xfe][..]));
        let units = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        assert_eq!(String::from_utf16(&units).unwrap(), "new\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn transcript_persistence_disable_does_not_disable_file_write() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _bootstrap = BootstrapRestore::capture();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::bootstrap::state::set_session_persistence_disabled(true);
        let root = std::env::temp_dir().join(format!(
            "cometix-write-no-session-persistence-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let path = root.join("new.txt");
        let request = request(&path, "written");
        let result = FileWriteTool
            .call(
                &request.input,
                &request,
                &ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(result.data, ToolOutput::Write(_)));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "written");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn no_write_gate_fails_before_creating_parent_or_history() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-disabled-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let path = root.join("nested/new.txt");
        let request = request(&path, "blocked");
        let result = FileWriteTool
            .call(
                &request.input,
                &request,
                &ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;
        let ToolOutput::Composed {
            content, status, ..
        } = result.data
        else {
            panic!("disabled write must fail")
        };
        assert_eq!(status, ToolResultStatus::Error);
        assert_eq!(
            content,
            format!(
                "<tool_use_error>{}</tool_use_error>",
                crate::tools::shared::write_gate::FILE_WRITE_DISABLED_ERROR
            )
        );
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn no_write_gate_leaves_existing_file_history_and_temp_state_untouched() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _bootstrap = BootstrapRestore::capture();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-disabled-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_dir = root.join("config");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("existing.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut app_state = crate::state::app_state_store::AppState::default();
        app_state.file_history =
            std::sync::Arc::new(crate::utils::file_history::FileHistoryState {
                snapshots: vec![crate::utils::file_history::FileHistorySnapshot {
                    message_id: "user-message".to_string(),
                    tracked_file_backups: std::collections::BTreeMap::new(),
                    timestamp: chrono::Utc::now(),
                }],
                ..crate::utils::file_history::FileHistoryState::default()
            });
        let store = crate::state::store::AppStore::new(app_state, None);
        let mut context = ToolUseContext::default().with_app_store(store.clone());
        context
            .read_file_state
            .set_entry(read_state(&path, "old\n"));
        let request = request(&path, "new\n");

        let result = FileWriteTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        assert!(matches!(
            result.data,
            ToolOutput::Composed {
                status: ToolResultStatus::Error,
                ..
            }
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
        assert!(
            store.get().file_history.snapshots[0]
                .tracked_file_backups
                .is_empty()
        );
        assert!(!config_dir.join("file-history").exists());
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn existing_write_tracks_file_history_through_real_app_store() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _bootstrap = BootstrapRestore::capture();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _checkpointing = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_dir = root.join("config");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("existing.txt");
        std::fs::write(&path, "old history\n").unwrap();
        let session_id = format!("write-history-{}", uuid::Uuid::new_v4().simple());
        crate::bootstrap::state::set_session_id(&session_id);
        crate::bootstrap::state::set_original_cwd(&root);
        crate::bootstrap::state::set_is_interactive(true);
        crate::utils::session_storage::reset_session_file_pointer();
        let session_file = root.join("session.jsonl");
        std::fs::write(&session_file, "").unwrap();
        crate::utils::session_storage::set_current_session_metadata_for_test(
            None,
            None,
            None,
            Some(session_file.clone()),
        );

        let mut app_state = crate::state::app_state_store::AppState::default();
        app_state.file_history =
            std::sync::Arc::new(crate::utils::file_history::FileHistoryState {
                snapshots: vec![crate::utils::file_history::FileHistorySnapshot {
                    message_id: "user-message".to_string(),
                    tracked_file_backups: std::collections::BTreeMap::new(),
                    timestamp: chrono::Utc::now(),
                }],
                ..crate::utils::file_history::FileHistoryState::default()
            });
        let store = crate::state::store::AppStore::new(app_state, None);
        let mut context = ToolUseContext::default().with_app_store(store.clone());
        context
            .read_file_state
            .set_entry(read_state(&path, "old history\n"));
        let request = request(&path, "new history\n");

        let parent_message = AssistantMessage {
            // The ENVELOPE uuid is what CC passes to `fileHistoryTrackEdit`
            // (`FileWriteTool.ts:261`). This fixture used to put the named value
            // on the identity block and leave the envelope random, so the
            // `messageId` assertion below pinned the identity uuid — the exact
            // behaviour CC does not have.
            uuid: "parent-message".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::MessageIdentity(
                crate::types::message::AssistantMessageIdentity {
                    request_id: None,
                    api_message_id: None,
                    ..Default::default()
                },
            )],
            model: None,
            stop_reason: None,
            usage: None,
        };
        let result = FileWriteTool
            .call(
                &request.input,
                &request,
                &context,
                None,
                Some(&parent_message),
                None,
            )
            .await;
        assert!(matches!(result.data, ToolOutput::Write(_)));
        let state = store.get();
        let backup = state.file_history.snapshots[0]
            .tracked_file_backups
            .get("existing.txt")
            .expect("Write tracks the edited file");
        let backup_path = config_dir
            .join("file-history")
            .join(&session_id)
            .join(backup.backup_file_name.as_deref().unwrap());
        assert_eq!(
            std::fs::read_to_string(backup_path).unwrap(),
            "old history\n"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new history\n");
        crate::utils::session_storage::flush_session_storage()
            .await
            .unwrap();
        let persisted = std::fs::read_to_string(&session_file).unwrap();
        let history_entry = persisted
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|entry| entry["type"] == "file-history-snapshot")
            .expect("Write persists the file-history snapshot update");
        assert_eq!(history_entry["messageId"], "parent-message");
        assert_eq!(history_entry["isSnapshotUpdate"], true);
        assert_eq!(
            history_entry["snapshot"]["trackedFileBackups"]["existing.txt"]["version"],
            1
        );

        crate::utils::session_storage::reset_session_file_pointer();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn remote_git_diff_gate_reads_switch_table_not_growthbook_cache() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _remote = EnvVarGuard::set("CLAUDE_CODE_REMOTE", "1");
        let _config_restore = TestGlobalConfigRestore;
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_quartz_lantern".to_string(),
            serde_json::Value::Bool(true),
        )]));
        config.growth_book_overrides = Some(std::collections::HashMap::from([(
            "tengu_quartz_lantern".to_string(),
            serde_json::Value::Bool(true),
        )]));
        crate::utils::config::set_test_global_config(Some(config));
        let root = std::env::temp_dir().join(format!(
            "cometix-write-remote-diff-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:owner/repository.git",
        ]);
        let path = root.join("new.txt");
        let request = request(&path, "one\ntwo\n");
        let mut context = ToolUseContext::default();
        context.cwd_override = Some(root.clone());

        let result = FileWriteTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let ToolOutput::Write(output) = result.data else {
            panic!("expected successful Write")
        };
        assert!(!remote_git_diff_enabled());
        assert!(
            output.git_diff.is_none(),
            "cached/override GrowthBook delivery must not open the remote diff gate"
        );

        // The attachment payload itself stays covered: the same file still
        // produces the official single-file diff once the gate is on.
        let diff = crate::utils::git_diff::fetch_single_file_git_diff(&path)
            .await
            .expect("remote diff payload");
        assert_eq!(
            diff.status,
            crate::utils::git_diff::ToolUseDiffStatus::Added
        );
        assert_eq!(diff.repository.as_deref(), Some("owner/repository"));
        assert_eq!((diff.additions, diff.deletions), (2, 0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_consumers_match_official_write_contract() {
        let input = serde_json::json!({
            "file_path": "relative.txt",
            "content": "hello"
        });
        assert_eq!(
            FileWriteTool.search_hint(),
            Some("create or overwrite files")
        );
        assert_eq!(
            FileWriteTool.get_path(&input).as_deref(),
            Some("relative.txt")
        );
        assert_eq!(
            FileWriteTool.to_auto_classifier_input(&input),
            "relative.txt: hello"
        );
        let schema = file_write_tool_schema();
        assert_eq!(schema.strict, Some(true));
        assert_eq!(schema.input_schema["additionalProperties"], false);
        assert!(
            schema.input_schema["properties"]["file_path"]
                .get("minLength")
                .is_none()
        );
        assert!(schema.aliases.is_empty());
        assert_eq!(ui::count_lines(""), 1);
        assert_eq!(ui::count_lines("one\n"), 1);
        assert_eq!(ui::count_lines("one\ntwo"), 2);
        // CC FileWriteTool.ts:99-101 + :104-107.
        assert_eq!(
            FileWriteTool.description(&serde_json::json!({})),
            "Write a file to the local filesystem."
        );
        let cwd = crate::bootstrap::state::get_original_cwd();
        assert_eq!(
            FileWriteTool.get_activity_description(&serde_json::json!({
                "file_path": cwd.join("src/foo.rs").to_string_lossy(),
                "content": "x"
            })),
            Some("Writing src/foo.rs".to_string())
        );
        assert_eq!(
            FileWriteTool.get_activity_description(&serde_json::json!({})),
            Some("Writing file".to_string())
        );
        // CC FileWriteTool.ts:146-152 extractSearchText() => '' opt-out.
        assert_eq!(
            FileWriteTool.extract_search_text(&ToolOutput::Composed {
                content: String::new(),
                status: crate::types::message::ToolResultStatus::Success,
            }),
            Some(String::new())
        );
    }

    /// Maps to: CC `utils/api.ts:646-660` — the Write branch strips trailing
    /// whitespace except for markdown (two trailing spaces are a hard break),
    /// with a case-insensitive suffix test.
    #[test]
    fn normalize_input_strips_trailing_whitespace_except_markdown_like_official() {
        use crate::tool::ToolCall as _;
        let normalize = |path: &str| {
            FileWriteTool.normalize_input(&serde_json::json!({
                "file_path": path,
                "content": "line one  \nline two\t\nend"
            }))["content"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(normalize("/repo/src/main.rs"), "line one\nline two\nend");
        assert_eq!(normalize("/repo/README.md"), "line one  \nline two\t\nend");
        assert_eq!(normalize("/repo/doc.MDX"), "line one  \nline two\t\nend");
        // Unparseable input passes through untouched.
        let unparseable = serde_json::json!({"content": "x  "});
        assert_eq!(FileWriteTool.normalize_input(&unparseable), unparseable);
    }

    /// Maps to: CC `UI.tsx:99-109` — only `create` truncates; the scan stops
    /// at the (MAX+1)th line and a trailing EOL is a terminator.
    #[test]
    fn is_result_truncated_matches_official_create_only_scan() {
        use crate::tool::ToolCall as _;
        let output = |kind: WriteOutputKind, content: &str| {
            ToolOutput::Write(WriteOutput {
                kind,
                file_path: "/tmp/f.txt".to_string(),
                content: content.to_string(),
                structured_patch: Vec::new(),
                original_file: None,
                read_timestamp_ms: 0,
                git_diff: None,
                dynamic_skill_dirs: Vec::new(),
            })
        };
        let eleven_lines = (0..11)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(FileWriteTool.is_result_truncated(&output(WriteOutputKind::Create, &eleven_lines)));
        // Update never truncates, even with the same content.
        assert!(
            !FileWriteTool.is_result_truncated(&output(WriteOutputKind::Update, &eleven_lines))
        );
        let ten_lines = (0..10)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!FileWriteTool.is_result_truncated(&output(WriteOutputKind::Create, &ten_lines)));
        // Ten lines plus a trailing EOL: the terminator is not an 11th line.
        let ten_with_eol = format!("{ten_lines}\n");
        assert!(
            !FileWriteTool.is_result_truncated(&output(WriteOutputKind::Create, &ten_with_eol))
        );
    }

    #[test]
    fn relative_permission_rules_use_tool_context_cwd_override() {
        let root = std::env::temp_dir().join("cometix-write-permission-cwd");
        let mut context = ToolUseContext::default();
        context.cwd_override = Some(root);
        context.tool_permission_context.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Edit",
                Some("nested/**".to_string()),
            )],
        );
        let input = serde_json::json!({
            "file_path": "nested/file.txt",
            "content": "blocked"
        });
        assert!(matches!(
            FileWriteTool.validate_input(&input, &context),
            ValidationResult::Error { error_code: 1, .. }
        ));
    }

    #[test]
    fn backfill_expands_only_the_observable_clone() {
        let root = std::env::temp_dir().join("cometix-write-backfill");
        let mut context = ToolUseContext::default();
        context.cwd_override = Some(root.clone());
        let original = serde_json::json!({"file_path": "nested.txt", "content": "x"});
        let observable = FileWriteTool.backfill_observable_input(&original, &context);
        assert_eq!(original["file_path"], "nested.txt");
        assert_eq!(
            observable["file_path"],
            root.join("nested.txt").display().to_string()
        );
    }

    #[tokio::test]
    async fn empty_existing_file_returns_create_like_official_truthiness_branch() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-empty-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("empty.txt");
        std::fs::write(&path, "").unwrap();
        let mut context = ToolUseContext::default();
        context.read_file_state.set_entry(read_state(&path, ""));
        let request = request(&path, "filled");
        let result = FileWriteTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let ToolOutput::Write(output) = result.data else {
            panic!("expected Write output")
        };
        assert_eq!(output.kind, WriteOutputKind::Create);
        assert!(output.original_file.is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
