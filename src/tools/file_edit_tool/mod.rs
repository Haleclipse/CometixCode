//! FileEditTool — exact in-place string replacement.
//!
//! Maps to: CC `tools/FileEditTool/FileEditTool.ts:1-625`.
//! Input/output helpers, prompt, and UI remain in their same-named modules;
//! file metadata, atomic writes, permission policy, history, Git diff, IDE/LSP
//! diagnostics, TEAMMEM, and dynamic skills remain in source-shaped owners.

pub mod constants;
pub mod prompt;
pub mod types;
pub mod ui;
pub mod utils;

pub use constants::FILE_EDIT_TOOL_NAME;
pub use types::{
    EditErrorOutput, EditInput, FileEdit, FileEditInput, FileEditOutput, GitDiffStatus,
    GitDiffSummary,
};

use constants::{FILE_UNEXPECTEDLY_MODIFIED_ERROR, MAX_EDIT_FILE_SIZE};
use futures::FutureExt as _;
use std::path::{Path, PathBuf};
use utils::{find_actual_string, get_patch_for_edit, preserve_quote_style};

/// Compatibility alias for older call sites. Maps to CC `FileEditOutput`.
pub(crate) type EditOutput = FileEditOutput;

/// Private `PermissionRequest.call_input` transport. `call_input` is
/// `#[serde(skip)]`, so this physical destination never reaches SDK/session
/// payloads.
pub(crate) const CHECKED_EDIT_DESTINATION_KEY: &str = "__cometix_checked_edit_destination";
pub(crate) const CHECKED_EDIT_APPROVED_PATH_KEY: &str = "__cometix_checked_edit_approved_path";

pub(crate) fn resolved_edit_destination(path: &Path) -> PathBuf {
    let candidate = crate::utils::fs_operations::get_paths_for_permission_check(path)
        .into_iter()
        .last()
        .unwrap_or_else(|| path.to_path_buf());
    crate::utils::path::expand_path(&candidate.display().to_string(), path.parent())
        .unwrap_or(candidate)
}

fn expected_edit_destination(
    request: &crate::types::permissions::PermissionRequest,
    context: &crate::tool::ToolUseContext,
) -> Option<PathBuf> {
    let call_input = request.call_input.as_ref()?;
    let original_path = call_input.get("file_path")?.as_str()?;
    let approved_path = call_input
        .get(CHECKED_EDIT_APPROVED_PATH_KEY)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(original_path);
    let approved_path =
        crate::utils::path::expand_path(approved_path, Some(&context.effective_cwd())).ok()?;
    let execution_path = request.input.get("file_path")?.as_str()?;
    let execution_path =
        crate::utils::path::expand_path(execution_path, Some(&context.effective_cwd())).ok()?;
    let destination = call_input
        .get(CHECKED_EDIT_DESTINATION_KEY)?
        .as_str()
        .map(PathBuf::from)?;
    if execution_path != approved_path {
        // Hooks run after the current Rust permission gate. Until the shared
        // gate is reordered like CC, fail closed against the last path the
        // user/permission engine actually approved.
        return Some(destination);
    }
    Some(destination)
}

fn is_non_regular_existing_path(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && !metadata.is_file())
}

/// Maps to: CC `FileEditTool/types.ts:6-19` `inputSchema`.
///
/// `replace_all` is `semanticBoolean(z.boolean().default(false).optional())`:
/// the default sits INSIDE the optional, so zod leaves the field out of
/// `required` while still emitting `default: false` — the mirror image of
/// ToolSearch's `.optional().default(5)`, which stays required.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "file_path",
                zod::string().describe("The absolute path to the file to modify"),
            ),
            ("old_string", zod::string().describe("The text to replace")),
            (
                "new_string",
                zod::string()
                    .describe("The text to replace it with (must be different from old_string)"),
            ),
            (
                "replace_all",
                crate::utils::semantic_boolean::semantic_boolean(
                    zod::boolean().default(serde_json::json!(false)).optional(),
                )
                .describe("Replace all occurrences of old_string (default false)"),
            ),
        ])
    })
}

/// Maps to CC `FileEditTool.inputSchema`.
pub fn file_edit_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: FILE_EDIT_TOOL_NAME.to_string(),
        description: prompt::get_edit_tool_description(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        strict: Some(true),
        ..Default::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditValidationError {
    pub(crate) message: String,
    pub(crate) error_code: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditValidationOk {
    pub(crate) actual_old_string: Option<String>,
}

fn validation_error(message: impl Into<String>, error_code: i32) -> EditValidationError {
    EditValidationError {
        message: message.into(),
        error_code,
    }
}

fn simulated_settings_edit(
    file: &str,
    actual_old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> String {
    if replace_all {
        file.replace(actual_old_string, new_string)
    } else {
        file.replacen(actual_old_string, new_string, 1)
    }
}

/// Maps to CC `FileEditTool.validateInput(...)` (`FileEditTool.ts:137-361`).
pub(crate) fn validate_edit_input(
    input: &FileEditInput,
    context: &crate::tool::ToolUseContext,
) -> Result<EditValidationOk, EditValidationError> {
    let cwd = context.effective_cwd();
    let full_path = crate::utils::path::expand_path(&input.file_path, Some(&cwd))
        .map_err(|error| validation_error(error, 2))?;
    let full_path_string = full_path.display().to_string();

    if let Err(error) =
        crate::services::team_memory_sync::team_mem_secret_guard::check_team_mem_secrets(
            &full_path,
            &input.new_string,
        )
    {
        return Err(validation_error(error, 0));
    }
    if input.old_string == input.new_string {
        return Err(validation_error(
            "No changes to make: old_string and new_string are exactly the same.",
            1,
        ));
    }

    if crate::utils::permissions::filesystem::matching_rule_for_input(
        &full_path_string,
        &context.tool_permission_context,
        crate::utils::permissions::filesystem::FilePermissionType::Edit,
        crate::types::permissions::PermissionBehavior::Deny,
        &cwd,
    )
    .is_some()
    {
        return Err(validation_error(
            "File is in a directory that is denied by your permission settings.",
            2,
        ));
    }

    // CC deliberately avoids filesystem access for UNC paths before the
    // permission layer to prevent NTLM credential leakage.
    if full_path_string.starts_with("//") || full_path_string.starts_with("\\\\") {
        return Ok(EditValidationOk::default());
    }

    let fs = crate::utils::fs_operations::get_fs_implementation();
    match futures::executor::block_on(fs.stat(&full_path)) {
        Ok(metadata) => {
            if !metadata.is_file() {
                // DEVIATION(SECURITY): CC reaches a later read failure for
                // special files. Reject before opening FIFO/device paths.
                return Err(validation_error("Cannot edit a non-regular file.", 2));
            }
            if metadata.len() > MAX_EDIT_FILE_SIZE {
                return Err(validation_error(
                    format!(
                        "File is too large to edit ({}). Maximum editable file size is {}.",
                        crate::utils::format::format_file_size(metadata.len()),
                        crate::utils::format::format_file_size(MAX_EDIT_FILE_SIZE)
                    ),
                    10,
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(validation_error(error.to_string(), 2)),
    }

    let file_content = match futures::executor::block_on(fs.read_file_bytes(&full_path, None)) {
        Ok(bytes) => {
            let decoded = if bytes.starts_with(&[0xff, 0xfe]) {
                String::from_utf16_lossy(
                    &bytes
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect::<Vec<_>>(),
                )
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            Some(decoded.replace("\r\n", "\n"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(validation_error(error.to_string(), 2)),
    };

    let Some(file_content) = file_content else {
        if input.old_string.is_empty() {
            return Ok(EditValidationOk::default());
        }
        let cwd_suggestion = crate::utils::file::suggest_path_under_cwd(&full_path, &cwd);
        let similar_file = crate::utils::file::find_similar_file(&full_path);
        let mut message = format!(
            "File does not exist. {} {}.",
            crate::utils::file::FILE_NOT_FOUND_CWD_NOTE,
            cwd.display()
        );
        if let Some(suggestion) = cwd_suggestion {
            message.push_str(&format!(" Did you mean {}?", suggestion.display()));
        } else if let Some(suggestion) = similar_file {
            message.push_str(&format!(" Did you mean {suggestion}?"));
        }
        return Err(validation_error(message, 4));
    };

    if input.old_string.is_empty() {
        if !file_content.trim().is_empty() {
            return Err(validation_error(
                "Cannot create new file - file already exists.",
                3,
            ));
        }
        return Ok(EditValidationOk::default());
    }

    if full_path_string.ends_with(".ipynb") {
        return Err(validation_error(
            format!(
                "File is a Jupyter Notebook. Use the {} to edit this file.",
                crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME
            ),
            5,
        ));
    }

    let Some(read_state) = context.read_file_state.get(&full_path) else {
        return Err(validation_error(
            "File has not been read yet. Read it first before writing to it.",
            6,
        ));
    };
    // Maps to CC `FileEditTool.validateInput`: auto-injected partial views do
    // not satisfy Read-before-Edit; ordinary ranged Read entries are still
    // accepted because `isPartialView` is the canonical safety signal.
    if read_state.is_partial_view {
        return Err(validation_error(
            "File has not been read yet. Read it first before writing to it.",
            6,
        ));
    }

    let last_write_time = crate::utils::file::get_file_modification_time_result(&full_path)
        .map_err(|error| validation_error(error.to_string(), 7))?;
    if last_write_time > read_state.timestamp_ms.unwrap_or(0) {
        // Windows timestamps can change without content changes, so full reads
        // fall back to a content comparison. Ranged reads always fail closed.
        let is_full_read = read_state.offset.is_none() && read_state.limit.is_none();
        if !(is_full_read && read_state.content.as_deref() == Some(file_content.as_str())) {
            return Err(validation_error(
                "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it.",
                7,
            ));
        }
    }

    let Some(actual_old_string) = find_actual_string(&file_content, &input.old_string) else {
        return Err(validation_error(
            format!(
                "String to replace not found in file.\nString: {}",
                input.old_string
            ),
            8,
        ));
    };

    let matches = file_content.matches(&actual_old_string).count();
    if matches > 1 && !input.replace_all {
        return Err(validation_error(
            format!(
                "Found {matches} matches of the string to replace, but replace_all is false. To replace all occurrences, set replace_all to true. To replace only one occurrence, please provide more context to uniquely identify the instance.\nString: {}",
                input.old_string
            ),
            9,
        ));
    }

    let updated_settings = simulated_settings_edit(
        &file_content,
        &actual_old_string,
        &input.new_string,
        input.replace_all,
    );
    if let Some(error) =
        crate::utils::settings::validate_edit_tool::validate_input_for_settings_file_edit(
            &full_path,
            &file_content,
            &updated_settings,
            &cwd,
        )
    {
        return Err(validation_error(error, 10));
    }

    Ok(EditValidationOk {
        actual_old_string: Some(actual_old_string),
    })
}

/// Maps to `getFeatureValue_CACHED_MAY_BE_STALE('tengu_quartz_lantern', false)`,
/// resolved from the source-controlled switch table instead of GrowthBook.
fn remote_git_diff_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::RemoteGitDiff,
    )
}

fn edit_error(
    message: impl Into<String>,
    dynamic_skill_dirs: Vec<String>,
) -> crate::tool::ToolResult {
    let content = message.into();
    crate::tool::ToolResult {
        data: crate::tool::ToolOutput::EditError(EditErrorOutput {
            content,
            dynamic_skill_dirs,
        }),
        new_messages: Vec::new(),
    }
}

fn parsed_input_to_edits(input: &FileEditInput) -> [FileEdit; 1] {
    [FileEdit {
        old_string: input.old_string.clone(),
        new_string: input.new_string.clone(),
        replace_all: input.replace_all,
    }]
}

pub(crate) struct FileEditTool;

impl crate::tool::ToolCall for FileEditTool {
    fn name(&self) -> &'static str {
        FILE_EDIT_TOOL_NAME
    }

    /// Maps to: CC `FileEditTool.ts:94-96` `async prompt() { return
    /// getEditToolDescription() }` — same source the wire schema renders.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_edit_tool_description()
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("modify file contents in place")
    }

    /// Maps to: CC `FileEditTool.ts:90-92` `description()` — the constant
    /// permission-dialog line; the input is not read.
    fn description(&self, _args: &serde_json::Value) -> String {
        "A tool for editing files".to_string()
    }

    /// Maps to: CC `FileEditTool.ts:96` mounting `UI.tsx:28-52#userFacingName`
    /// (`Updated plan` / `Update` / `Create`), which `buildTool` puts on the
    /// Tool object for `toolUseConfirm.tool.userFacingName(input)`.
    fn user_facing_name(&self, args: Option<&serde_json::Value>) -> String {
        crate::tools::file_edit_tool::ui::user_facing_name(args)
    }

    /// Maps to: CC `FileEditTool.ts:97` mounting `UI.tsx#getToolUseSummary`.
    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        crate::tools::file_edit_tool::ui::get_tool_use_summary(Some(args))
    }

    /// Maps to: CC `FileEditTool.ts:98-101` `getActivityDescription(input)`.
    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            crate::tools::file_edit_tool::ui::get_tool_use_summary(Some(args))
                .map(|summary| format!("Editing {summary}"))
                .unwrap_or_else(|| "Editing file".to_string()),
        )
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to the semanticBoolean part of CC `inputSchema`.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut parsed = args.clone();
        crate::utils::semantic_boolean::preprocess_object_field(&mut parsed, "replace_all");
        parsed
    }

    /// Maps to CC `utils/api.ts#normalizeToolInput` Edit branch.
    fn normalize_input_with_context(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> serde_json::Value {
        let mut parsed = self.normalize_input(args);
        let Ok(input) = FileEditInput::from_args(&parsed) else {
            return parsed;
        };
        let edits = [EditInput {
            old_string: input.old_string.clone(),
            new_string: input.new_string.clone(),
            replace_all: input.replace_all,
        }];
        let (_, normalized_edits) =
            utils::normalize_file_edit_input(&input.file_path, &edits, &context.effective_cwd());
        let Some(edit) = normalized_edits.first() else {
            return parsed;
        };
        if let Some(object) = parsed.as_object_mut() {
            object.insert(
                "old_string".to_string(),
                serde_json::Value::String(edit.old_string.clone()),
            );
            object.insert(
                "new_string".to_string(),
                serde_json::Value::String(edit.new_string.clone()),
            );
            object.insert(
                "replace_all".to_string(),
                serde_json::Value::Bool(edit.replace_all),
            );
        }
        parsed
    }

    fn backfill_observable_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> serde_json::Value {
        let Ok(input) = FileEditInput::from_args(args) else {
            return args.clone();
        };
        let Ok(full_path) =
            crate::utils::path::expand_path(&input.file_path, Some(&context.effective_cwd()))
        else {
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

    fn inputs_equivalent(
        &self,
        left: &serde_json::Value,
        right: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> Result<Option<bool>, String> {
        let Ok(left) = FileEditInput::from_args(left) else {
            return Ok(None);
        };
        let Ok(right) = FileEditInput::from_args(right) else {
            return Ok(None);
        };
        utils::are_file_edits_inputs_equivalent(
            &left.file_path,
            &parsed_input_to_edits(&left),
            &right.file_path,
            &parsed_input_to_edits(&right),
            &context.effective_cwd(),
        )
        .map(Some)
        .map_err(|error| error.to_string())
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        match FileEditInput::from_args(args)
            .map_err(|message| validation_error(message, 1))
            .and_then(|input| validate_edit_input(&input, context))
        {
            Ok(_) => crate::tool::ValidationResult::Ok,
            Err(error) => crate::tool::ValidationResult::Error {
                message: error.message,
                error_code: error.error_code,
            },
        }
    }

    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let path = FileEditInput::from_args(args)
            .and_then(|input| {
                crate::utils::path::expand_path(&input.file_path, Some(&context.effective_cwd()))
            })
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
        FileEditInput::from_args(args)
            .ok()
            .map(|input| input.file_path)
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        FileEditInput::from_args(args)
            .map(|input| format!("{}: {}", input.file_path, input.new_string))
            .unwrap_or_default()
    }

    fn prepare_permission_matcher(
        &self,
        args: &serde_json::Value,
    ) -> Option<crate::tool::PermissionPatternMatcher> {
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
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        async move {
            let input = match FileEditInput::from_args(args) {
                Ok(input) => input,
                Err(error) => return edit_error(error, Vec::new()),
            };
            let full_path = match crate::utils::path::expand_path(
                &input.file_path,
                Some(&context.effective_cwd()),
            ) {
                Ok(path) => path,
                Err(error) => return edit_error(error, Vec::new()),
            };

            // DEVIATION(SECURITY): repository-wide no-write mode has no CC
            // equivalent. It must fail before skills, mkdir, history, or temp.
            if !crate::utils::env_utils::is_cometix_write_enabled() {
                return edit_error(
                    crate::tools::shared::write_gate::FILE_EDIT_DISABLED_ERROR,
                    Vec::new(),
                );
            }

            let cwd = context.effective_cwd();
            // Snapshot the user/permission-approved physical destination before
            // any skill/diagnostic/history await; final critical re-resolution
            // below must remain identical.
            let write_target = resolved_edit_destination(&full_path);
            if expected_edit_destination(request, context)
                .is_some_and(|expected| expected != write_target)
            {
                return edit_error(FILE_UNEXPECTEDLY_MODIFIED_ERROR, Vec::new());
            }
            let touched_paths = [full_path.clone()];
            let mut dynamic_skill_dirs = Vec::new();
            if !crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
            ) {
                let skill_dirs = crate::skills::load_skills_dir::discover_skill_dirs_for_paths(
                    &touched_paths,
                    &cwd,
                );
                dynamic_skill_dirs = skill_dirs
                    .iter()
                    .map(|directory| directory.display().to_string())
                    .collect();
                if let Some(triggers) = &context.dynamic_skill_dir_triggers {
                    triggers.extend(dynamic_skill_dirs.iter().cloned());
                }
                // DEVIATION(L1): CC starts this loader without awaiting. The
                // Rust loader is synchronous so inline loading guarantees the
                // same set is available on the next turn.
                crate::skills::load_skills_dir::add_skill_directories(&skill_dirs);
                crate::skills::load_skills_dir::activate_conditional_skills_for_paths(
                    &touched_paths,
                    &cwd,
                );
            }

            crate::services::diagnostic_tracking::before_file_edited(
                &full_path.display().to_string(),
            )
            .await;

            if is_non_regular_existing_path(&write_target) {
                return edit_error("Cannot edit a non-regular file.", dynamic_skill_dirs);
            }
            if let Err(error) = crate::utils::fs_operations::get_fs_implementation()
                .mkdir(
                    write_target
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new(".")),
                    None,
                )
                .await
            {
                return edit_error(error.to_string(), dynamic_skill_dirs);
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

            // Final await above. Do not yield between this re-resolution,
            // freshness check, patch application, and atomic write.
            let critical_target = resolved_edit_destination(&full_path);
            if critical_target != write_target {
                return edit_error(FILE_UNEXPECTEDLY_MODIFIED_ERROR, dynamic_skill_dirs);
            }
            if is_non_regular_existing_path(&critical_target) {
                return edit_error("Cannot edit a non-regular file.", dynamic_skill_dirs);
            }

            let ReadFileForEdit {
                content: original_file,
                file_exists,
                encoding,
                line_endings,
            } = match read_file_for_edit(&critical_target) {
                Ok(meta) => meta,
                Err(error) => return edit_error(error.to_string(), dynamic_skill_dirs),
            };

            if file_exists {
                let last_write_time =
                    match crate::utils::file::get_file_modification_time_result(&critical_target) {
                        Ok(timestamp) => timestamp,
                        Err(error) => return edit_error(error.to_string(), dynamic_skill_dirs),
                    };
                let last_read = context.read_file_state.get(&full_path);
                if last_read
                    .as_ref()
                    .is_none_or(|entry| last_write_time > entry.timestamp_ms.unwrap_or(0))
                {
                    let content_unchanged = last_read.as_ref().is_some_and(|entry| {
                        entry.offset.is_none()
                            && entry.limit.is_none()
                            && entry.content.as_deref() == Some(original_file.as_str())
                    });
                    if !content_unchanged {
                        return edit_error(FILE_UNEXPECTEDLY_MODIFIED_ERROR, dynamic_skill_dirs);
                    }
                }
            }

            let actual_old_string = find_actual_string(&original_file, &input.old_string)
                .unwrap_or_else(|| input.old_string.clone());
            let actual_new_string =
                preserve_quote_style(&input.old_string, &actual_old_string, &input.new_string);
            let (patch, updated_file) = match get_patch_for_edit(
                &original_file,
                &actual_old_string,
                &actual_new_string,
                input.replace_all,
            ) {
                Ok(result) => result,
                Err(error) => return edit_error(error, dynamic_skill_dirs),
            };

            if let Err(error) = crate::utils::file::write_text_content_to_target(
                &critical_target,
                &updated_file,
                encoding,
                line_endings,
            ) {
                return edit_error(error.to_string(), dynamic_skill_dirs);
            }
            // Maps to: CC `FileEditTool.ts:494-515` — the whole LSP block,
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
                let lsp_content = updated_file.clone();
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
                    if let Err(error) = crate::services::lsp::manager::save_file(&lsp_path).await {
                        crate::utils::debug::log_for_debugging(&format!(
                            "LSP: Failed to notify server of file save for {lsp_path}: {error}"
                        ));
                        // CC: logError(err) — joins with the error log.
                    }
                });
            }

            // Maps to: CC `FileEditTool.ts:518` notifyVscodeFileUpdated.
            let vscode_path = full_path.display().to_string();
            let vscode_original = original_file.clone();
            let vscode_updated = updated_file.clone();
            tokio::spawn(async move {
                let _ = crate::services::mcp::vscode_sdk_mcp::notify_vscode_file_updated(
                    &vscode_path,
                    Some(&vscode_original),
                    Some(&vscode_updated),
                )
                .await;
            });

            let read_timestamp_ms =
                match crate::utils::file::get_file_modification_time_result(&critical_target) {
                    Ok(timestamp) => timestamp,
                    Err(error) => return edit_error(error.to_string(), dynamic_skill_dirs),
                };
            // Maps to: CC `FileEditTool.ts:520-526` source-position
            // `readFileState.set(fullFilePath, ...)`.
            context
                .read_file_state
                .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                    path: full_path.display().to_string(),
                    content: Some(updated_file.clone()),
                    timestamp_ms: Some(read_timestamp_ms),
                    offset: None,
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::EditRefresh,
                });

            // 7. Log events (CC `FileEditTool.ts:528-543`).
            // CC: logEvent('tengu_write_claudemd', {}) on `${sep}CLAUDE.md`
            // paths — joins with analytics.
            crate::utils::diff::count_lines_changed(&patch, None);
            // CC: logFileOperation({operation: 'edit', tool: 'FileEditTool',
            // filePath}) — joins with analytics
            // (utils/fileOperationAnalytics.ts).
            // CC: logEvent('tengu_edit_string_lengths', {oldStringBytes,
            // newStringBytes, replaceAll}) — joins with analytics.

            let git_diff = if crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref(),
            ) && remote_git_diff_enabled()
            {
                // CC: logEvent('tengu_tool_use_diff_computed', {isEditTool,
                // durationMs, hasDiff}) after the fetch — joins with
                // analytics.
                crate::utils::git_diff::fetch_single_file_git_diff(&full_path).await
            } else {
                None
            };

            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::Edit(FileEditOutput {
                    file_path: input.file_path,
                    old_string: actual_old_string,
                    new_string: input.new_string,
                    original_file,
                    structured_patch: patch,
                    user_modified: context.user_modified.unwrap_or(false),
                    replace_all: input.replace_all,
                    git_diff,
                    updated_file,
                    read_timestamp_ms,
                    dynamic_skill_dirs,
                }),
                new_messages: Vec::new(),
            }
        }
        .boxed()
    }

    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::ToolResultStatus;
        match data {
            crate::tool::ToolOutput::Edit(output) => {
                let modified_note = if output.user_modified {
                    ".  The user modified your proposed changes before accepting them. "
                } else {
                    ""
                };
                if output.replace_all {
                    (
                        format!(
                            "The file {} has been updated{}. All occurrences were successfully replaced.",
                            output.file_path, modified_note
                        ),
                        ToolResultStatus::Success,
                    )
                } else {
                    (
                        format!(
                            "The file {} has been updated successfully{}.",
                            output.file_path, modified_note
                        ),
                        ToolResultStatus::Success,
                    )
                }
            }
            crate::tool::ToolOutput::EditError(output) => {
                (output.content.clone(), ToolResultStatus::Error)
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>Invalid Edit output</tool_use_error>".to_string(),
                ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording FileEditTool's `FileEditOutput` as the message's
    /// `toolUseResult` — the render layer parses it back with
    /// `ui::parse_output`. Error paths record CC's internal `"Error: ..."`
    /// `toolUseResult` string.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::Edit(output) => Some(ui::output_to_value(output)),
            crate::tool::ToolOutput::EditError(output) => {
                let error = crate::utils::messages::extract_tag(&output.content, "tool_use_error")
                    .unwrap_or_else(|| output.content.clone());
                Some(serde_json::Value::String(format!("Error: {error}")))
            }
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let error = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(format!("Error: {error}")))
            }
            _ => None,
        }
    }
}

/// Maps to: CC `FileEditTool.ts:599-604` — the return shape of
/// `readFileForEdit`.
struct ReadFileForEdit {
    content: String,
    file_exists: bool,
    encoding: crate::utils::file_read::FileEncoding,
    line_endings: crate::utils::file_read::LineEndingType,
}

/// Maps to: CC `FileEditTool.ts:599-625` `readFileForEdit`: ENOENT yields the
/// empty-file defaults; any other error rethrows to the caller.
fn read_file_for_edit(absolute_file_path: &std::path::Path) -> std::io::Result<ReadFileForEdit> {
    match crate::utils::file_read::read_file_sync_with_metadata(absolute_file_path) {
        Ok(metadata) => Ok(ReadFileForEdit {
            content: metadata.content,
            file_exists: true,
            encoding: metadata.encoding,
            line_endings: metadata.line_endings,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ReadFileForEdit {
            content: String::new(),
            file_exists: false,
            encoding: crate::utils::file_read::FileEncoding::Utf8,
            line_endings: crate::utils::file_read::LineEndingType::Lf,
        }),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{ToolCall as _, ToolOutput, ToolUseContext};
    use crate::utils::query_helpers::{ReadFileStateEntry, ReadFileStateSource};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn update_read_state(context: &ToolUseContext, update: impl FnOnce(&mut ReadFileStateEntry)) {
        let mut entries = context.read_file_state.snapshot();
        update(&mut entries[0]);
        context.read_file_state.replace(entries);
    }

    struct TestGlobalConfigRestore(Option<crate::utils::config::GlobalConfig>);

    impl TestGlobalConfigRestore {
        fn set(config: crate::utils::config::GlobalConfig) -> Self {
            Self(crate::utils::config::replace_test_global_config(Some(
                config,
            )))
        }
    }

    impl Drop for TestGlobalConfigRestore {
        fn drop(&mut self) {
            crate::utils::config::replace_test_global_config(self.0.take());
        }
    }

    struct BootstrapRestore {
        session_id: String,
        original_cwd: PathBuf,
        interactive: bool,
        persistence_disabled: bool,
    }

    impl BootstrapRestore {
        fn capture() -> Self {
            Self {
                session_id: crate::bootstrap::state::get_session_id(),
                original_cwd: crate::bootstrap::state::get_original_cwd(),
                interactive: crate::bootstrap::state::get_is_interactive(),
                persistence_disabled: crate::bootstrap::state::is_session_persistence_disabled(),
            }
        }
    }

    impl Drop for BootstrapRestore {
        fn drop(&mut self) {
            crate::bootstrap::state::set_session_id(&self.session_id);
            crate::bootstrap::state::set_original_cwd(&self.original_cwd);
            crate::bootstrap::state::set_is_interactive(self.interactive);
            crate::bootstrap::state::set_session_persistence_disabled(self.persistence_disabled);
            crate::utils::session_storage::reset_session_file_pointer();
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn seed_read_state(context: &mut ToolUseContext, path: &Path, content: &str) {
        context.read_file_state.set_entry(ReadFileStateEntry {
            path: path.display().to_string(),
            content: Some(content.to_string()),
            timestamp_ms: crate::utils::file::get_file_modification_time(path),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: ReadFileStateSource::Read,
        });
    }

    fn request(path: &Path, old: &str, new: &str) -> crate::types::permissions::PermissionRequest {
        crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-edit".to_string(),
            "toolu-edit".to_string(),
            FILE_EDIT_TOOL_NAME.to_string(),
            path.display().to_string(),
            serde_json::json!({
                "file_path": path.display().to_string(),
                "old_string": old,
                "new_string": new,
                "replace_all": false
            }),
            crate::types::permissions::PermissionMode::Default,
        )
    }

    #[test]
    fn file_edit_schema_and_metadata_match_official_contract() {
        let schema = file_edit_tool_schema();
        assert_eq!(schema.name, "Edit");
        assert_eq!(schema.strict, Some(true));
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["file_path", "old_string", "new_string"])
        );
        assert_eq!(schema.input_schema["additionalProperties"], false);
        assert_eq!(FileEditTool.name(), "Edit");
        assert!(FileEditTool.aliases().is_empty());
        assert_eq!(
            FileEditTool.search_hint(),
            Some("modify file contents in place")
        );
        assert_eq!(FileEditTool.max_result_size_chars(), 100_000);
        // CC FileEditTool.ts:90-92 + :98-101.
        assert_eq!(
            FileEditTool.description(&serde_json::json!({})),
            "A tool for editing files"
        );
        let cwd = crate::bootstrap::state::get_original_cwd();
        assert_eq!(
            FileEditTool.get_activity_description(&serde_json::json!({
                "file_path": cwd.join("src/foo.rs").to_string_lossy(),
                "old_string": "a",
                "new_string": "b"
            })),
            Some("Editing src/foo.rs".to_string())
        );
        assert_eq!(
            FileEditTool.get_activity_description(&serde_json::json!({})),
            Some("Editing file".to_string())
        );
        assert_eq!(
            FileEditTool
                .prepare_permission_matcher(&serde_json::json!({
                    "file_path": "/repo/src/lib.rs",
                    "old_string": "a",
                    "new_string": "b"
                }))
                .expect("matcher")("/repo/src/**"),
            true
        );
    }

    #[test]
    fn observable_path_and_context_normalization_preserve_model_call_path() {
        let root = temp_root("normalize");
        let path = root.join("src/code.rs");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "let value = “old”;\n").unwrap();
        let context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        let args = serde_json::json!({
            "file_path": "src/code.rs",
            "old_string": "let value = \"old\";",
            "new_string": "let value = \"new\";   \n",
            "replace_all": "false"
        });

        let observable = FileEditTool.backfill_observable_input(&args, &context);
        assert_eq!(observable["file_path"], path.display().to_string());
        assert_eq!(args["file_path"], "src/code.rs");
        let normalized = FileEditTool.normalize_input_with_context(&args, &context);
        assert_eq!(normalized["old_string"], "let value = \"old\";");
        assert_eq!(normalized["new_string"], "let value = \"new\";\n");
        assert_eq!(normalized["replace_all"], false);
        assert_eq!(normalized["file_path"], "src/code.rs");

        std::fs::write(root.join("equivalent.txt"), "hello world\n").unwrap();
        let narrow = serde_json::json!({
            "file_path": "equivalent.txt",
            "old_string": "world",
            "new_string": "there",
            "replace_all": false
        });
        let broad = serde_json::json!({
            "file_path": "equivalent.txt",
            "old_string": "hello world",
            "new_string": "hello there",
            "replace_all": false
        });
        assert_eq!(
            FileEditTool.inputs_equivalent(&narrow, &broad, &context),
            Ok(Some(true))
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relative_permission_rules_use_tool_context_cwd_override() {
        let root = temp_root("permission-cwd");
        let mut context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        context.tool_permission_context.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Edit",
                Some("nested/**".to_string()),
            )],
        );
        let input = FileEditInput {
            file_path: "nested/file.txt".to_string(),
            old_string: String::new(),
            new_string: "blocked".to_string(),
            replace_all: false,
        };
        let error = validate_edit_input(&input, &context).unwrap_err();
        assert_eq!(error.error_code, 2);
        assert_eq!(
            error.message,
            "File is in a directory that is denied by your permission settings."
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validation_read_lookup_promotes_cache_entry_like_official() {
        let root = temp_root("validation-cache-promotion");
        let path = root.join("sample.txt");
        let decoy = root.join("decoy.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut context = ToolUseContext::default();
        seed_read_state(&mut context, &path, "old\n");
        seed_read_state(&mut context, &decoy, "decoy\n");
        let input = FileEditInput {
            file_path: path.display().to_string(),
            old_string: "old".to_string(),
            new_string: "new".to_string(),
            replace_all: false,
        };

        validate_edit_input(&input, &context).unwrap();

        assert_eq!(
            context.read_file_state.keys(),
            vec![path.display().to_string(), decoy.display().to_string()]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validation_enforces_non_partial_read_unique_match_and_freshness() {
        let root = temp_root("validation");
        let path = root.join("sample.txt");
        std::fs::write(&path, "old\nold\n").unwrap();
        let input = FileEditInput {
            file_path: path.display().to_string(),
            old_string: "old".to_string(),
            new_string: "new".to_string(),
            replace_all: false,
        };
        let mut context = ToolUseContext::default();
        assert_eq!(
            validate_edit_input(&input, &context)
                .unwrap_err()
                .error_code,
            6
        );

        seed_read_state(&mut context, &path, "old\nold\n");
        update_read_state(&context, |entry| entry.offset = Some(serde_json::json!(1)));
        assert_eq!(
            validate_edit_input(&input, &context)
                .unwrap_err()
                .error_code,
            9
        );
        update_read_state(&context, |entry| entry.limit = Some(serde_json::json!(1)));
        assert_eq!(
            validate_edit_input(&input, &context)
                .unwrap_err()
                .error_code,
            9
        );
        update_read_state(&context, |entry| entry.is_partial_view = true);
        assert_eq!(
            validate_edit_input(&input, &context)
                .unwrap_err()
                .error_code,
            6
        );
        update_read_state(&context, |entry| {
            entry.is_partial_view = false;
            entry.offset = None;
            entry.limit = None;
        });
        assert_eq!(
            validate_edit_input(&input, &context)
                .unwrap_err()
                .error_code,
            9
        );

        let mut replace_all = input.clone();
        replace_all.replace_all = true;
        assert_eq!(
            validate_edit_input(&replace_all, &context)
                .unwrap()
                .actual_old_string
                .as_deref(),
            Some("old")
        );
        update_read_state(&context, |entry| {
            entry.timestamp_ms = Some(0);
            entry.content = Some("stale".to_string());
        });
        assert_eq!(
            validate_edit_input(&replace_all, &context)
                .unwrap_err()
                .error_code,
            7
        );

        let same = FileEditInput {
            new_string: "old".to_string(),
            ..input
        };
        assert_eq!(
            validate_edit_input(&same, &context).unwrap_err().error_code,
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn validation_blocks_secrets_introduced_into_team_memory() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let input = FileEditInput {
            file_path: crate::memdir::team_mem_paths::get_team_mem_path()
                .join("shared.md")
                .display()
                .to_string(),
            old_string: String::new(),
            new_string: format!("token: ghp_{}", "a".repeat(36)),
            replace_all: false,
        };
        let error = validate_edit_input(&input, &ToolUseContext::default()).unwrap_err();
        assert_eq!(error.error_code, 0);
        assert_eq!(
            error.message,
            "Content contains potential secrets (GitHub PAT) and cannot be written to team memory. Team memory is shared with all repository collaborators. Remove the sensitive content and try again."
        );
    }

    #[cfg(unix)]
    #[test]
    fn validation_rejects_fifo_without_opening_it() {
        use std::os::unix::ffi::OsStrExt as _;
        let root = temp_root("fifo");
        let path = root.join("pipe");
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        let input = FileEditInput {
            file_path: path.display().to_string(),
            old_string: "old".to_string(),
            new_string: "new".to_string(),
            replace_all: false,
        };
        let error = validate_edit_input(&input, &ToolUseContext::default()).unwrap_err();
        assert_eq!(error.error_code, 2);
        assert_eq!(error.message, "Cannot edit a non-regular file.");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validation_reports_similar_file_and_rejects_sparse_oversize_file() {
        let root = temp_root("errors");
        std::fs::write(root.join("config.rs"), "value\n").unwrap();
        let missing = FileEditInput {
            file_path: root.join("config.ts").display().to_string(),
            old_string: "value".to_string(),
            new_string: "next".to_string(),
            replace_all: false,
        };
        let error = validate_edit_input(&missing, &ToolUseContext::default()).unwrap_err();
        assert_eq!(error.error_code, 4);
        assert!(error.message.contains("Did you mean config.rs?"));

        let large = root.join("large.txt");
        let file = std::fs::File::create(&large).unwrap();
        file.set_len(MAX_EDIT_FILE_SIZE + 1).unwrap();
        drop(file);
        let oversized = FileEditInput {
            file_path: large.display().to_string(),
            old_string: "a".to_string(),
            new_string: "b".to_string(),
            replace_all: false,
        };
        let error = validate_edit_input(&oversized, &ToolUseContext::default()).unwrap_err();
        assert_eq!(error.error_code, 10);
        assert!(error.message.starts_with("File is too large to edit ("));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn settings_edit_validation_uses_official_error_code_ten() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = temp_root("settings");
        let path = root.join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "{\"defaultShell\":\"bash\"}\n";
        std::fs::write(&path, original).unwrap();
        let mut context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        seed_read_state(&mut context, &path, original);
        let input = FileEditInput {
            file_path: path.display().to_string(),
            old_string: "bash".to_string(),
            new_string: "fish".to_string(),
            replace_all: false,
        };
        let error = validate_edit_input(&input, &context).unwrap_err();
        assert_eq!(error.error_code, 10);
        assert!(
            error
                .message
                .starts_with("Claude Code settings.json validation failed after edit:")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn no_write_gate_fails_before_parent_creation() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let root = temp_root("disabled");
        let path = root.join("missing/disabled.txt");
        let request = request(&path, "", "content\n");
        let result = FileEditTool
            .call(
                &request.input,
                &request,
                &ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;
        let ToolOutput::EditError(output) = result.data else {
            panic!("expected EditError")
        };
        assert_eq!(
            output.content,
            crate::tools::shared::write_gate::FILE_EDIT_DISABLED_ERROR
        );
        assert!(!path.parent().unwrap().exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_old_string_creates_missing_file_and_replaces_whitespace_only_file() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let root = temp_root("empty-old");

        let missing = root.join("nested/new.txt");
        let missing_request = request(&missing, "", "created\n");
        let result = FileEditTool
            .call(
                &missing_request.input,
                &missing_request,
                &ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;
        let ToolOutput::Edit(output) = result.data else {
            panic!("missing file creation should succeed")
        };
        assert_eq!(output.original_file, "");
        assert_eq!(std::fs::read_to_string(&missing).unwrap(), "created\n");

        let whitespace = root.join("whitespace.txt");
        std::fs::write(&whitespace, "  \n").unwrap();
        let mut context = ToolUseContext::default();
        seed_read_state(&mut context, &whitespace, "  \n");
        let whitespace_request = request(&whitespace, "", "replacement\n");
        let result = FileEditTool
            .call(
                &whitespace_request.input,
                &whitespace_request,
                &context,
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(result.data, ToolOutput::Edit(_)));
        assert_eq!(
            std::fs::read_to_string(&whitespace).unwrap(),
            "replacement\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execution_preserves_utf16_bom_crlf_and_emits_typed_output() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let root = temp_root("encoding");
        let path = root.join("encoded.txt");
        let original = "alpha\r\nold\r\n";
        let mut bytes = vec![0xff, 0xfe];
        for unit in original.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        let mut context = ToolUseContext::default();
        seed_read_state(&mut context, &path, "\u{feff}alpha\nold\n");
        let request = request(&path, "old", "new");
        let result = FileEditTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let ToolOutput::Edit(output) = result.data else {
            panic!("expected typed Edit output")
        };
        assert_eq!(output.file_path, path.display().to_string());
        assert_eq!(output.old_string, "old");
        assert_eq!(output.new_string, "new");
        assert_eq!(output.original_file, "\u{feff}alpha\nold\n");
        assert_eq!(output.updated_file, "\u{feff}alpha\nnew\n");
        assert!(output.structured_patch.iter().any(|hunk| {
            hunk.lines.iter().any(|line| line == "-old")
                && hunk.lines.iter().any(|line| line == "+new")
        }));
        let written = std::fs::read(&path).unwrap();
        assert_eq!(&written[..2], &[0xff, 0xfe]);
        let metadata = crate::utils::file_read::read_file_sync_with_metadata(&path).unwrap();
        assert_eq!(metadata.content, "\u{feff}alpha\nnew\n");
        assert_eq!(
            metadata.encoding,
            crate::utils::file_read::FileEncoding::Utf16Le
        );
        assert_eq!(
            metadata.line_endings,
            crate::utils::file_read::LineEndingType::CrLf
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_all_updates_every_match_and_maps_official_result_copy() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let root = temp_root("replace-all");
        let path = root.join("values.txt");
        std::fs::write(&path, "old\nold\n").unwrap();
        let mut context = ToolUseContext::default();
        seed_read_state(&mut context, &path, "old\nold\n");
        let mut request = request(&path, "old", "new");
        request.input["replace_all"] = serde_json::Value::Bool(true);
        let result = FileEditTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let ToolOutput::Edit(output) = result.data else {
            panic!("expected Edit output")
        };
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\nnew\n");
        let (content, status) = FileEditTool
            .map_tool_result_to_tool_result_block_param(&ToolOutput::Edit(output), "toolu-edit");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert_eq!(
            content,
            format!(
                "The file {} has been updated. All occurrences were successfully replaced.",
                path.display()
            )
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execution_tracks_pre_edit_file_history_and_session_snapshot() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _bootstrap = BootstrapRestore::capture();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let _checkpointing = EnvVarGuard::set("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING", "0");
        let root = temp_root("history");
        let config_dir = root.join("config");
        let _config_dir = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        let _config_restore =
            TestGlobalConfigRestore::set(crate::utils::config::GlobalConfig::default());
        let path = root.join("existing.txt");
        std::fs::write(&path, "old history\n").unwrap();
        let session_id = format!("edit-history-{}", uuid::Uuid::new_v4().simple());
        crate::bootstrap::state::set_session_id(&session_id);
        crate::bootstrap::state::set_original_cwd(&root);
        crate::bootstrap::state::set_is_interactive(true);
        crate::bootstrap::state::set_session_persistence_disabled(false);
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
        seed_read_state(&mut context, &path, "old history\n");
        let request = request(&path, "old history", "new history");
        let parent_message = crate::types::message::AssistantMessage {
            // The ENVELOPE uuid — CC passes `parentMessage.uuid` to
            // `fileHistoryTrackEdit` (`FileWriteTool.ts:261`). The named value
            // used to sit on the identity block instead, so the `messageId`
            // assertion pinned the identity uuid.
            uuid: "parent-edit-message".to_string(),
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
        let result = FileEditTool
            .call(
                &request.input,
                &request,
                &context,
                None,
                Some(&parent_message),
                None,
            )
            .await;
        assert!(matches!(result.data, ToolOutput::Edit(_)));
        let state = store.get();
        let backup = state.file_history.snapshots[0]
            .tracked_file_backups
            .get("existing.txt")
            .expect("Edit tracks the pre-edit file");
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
            .expect("Edit persists the file-history snapshot update");
        assert_eq!(history_entry["messageId"], "parent-edit-message");
        assert_eq!(history_entry["isSnapshotUpdate"], true);

        crate::utils::session_storage::reset_session_file_pointer();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_git_diff_gate_reads_switch_table_not_growthbook_cache() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let _remote = EnvVarGuard::set("CLAUDE_CODE_REMOTE", "1");
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_quartz_lantern".to_string(),
            serde_json::Value::Bool(true),
        )]));
        config.growth_book_overrides = Some(std::collections::HashMap::from([(
            "tengu_quartz_lantern".to_string(),
            serde_json::Value::Bool(true),
        )]));
        let _config_restore = TestGlobalConfigRestore::set(config);
        let root = temp_root("remote-diff");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
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
        let path = root.join("value.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        seed_read_state(&mut context, &path, "old\n");
        let request = request(&path, "old", "new");
        let result = FileEditTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let ToolOutput::Edit(output) = result.data else {
            panic!("expected Edit output")
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
        assert_eq!((diff.additions, diff.deletions), (1, 0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn post_discovery_failure_preserves_dynamic_skill_trigger() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "0");
        let _skills = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let root = temp_root("dynamic-error");
        let package = root.join("package");
        let skill_dir = package.join(".claude/skills");
        std::fs::create_dir_all(skill_dir.join("edit-skill")).unwrap();
        std::fs::write(
            skill_dir.join("edit-skill/SKILL.md"),
            "---\nname: edit-skill\ndescription: Edit skill\n---\nBody",
        )
        .unwrap();
        let target = package.join("src/non-regular");
        std::fs::create_dir_all(&target).unwrap();
        let request = request(&target, "old", "new");
        let context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        let result = FileEditTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let ToolOutput::EditError(output) = result.data else {
            panic!("non-regular destination must fail")
        };
        assert!(
            output
                .dynamic_skill_dirs
                .contains(&skill_dir.display().to_string())
        );
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn approved_symlink_destination_retarget_fails_closed() {
        use std::os::unix::fs::symlink;
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let root = temp_root("retarget");
        let package = root.join("package");
        std::fs::create_dir_all(&package).unwrap();
        let target_a = package.join("a.txt");
        let target_b = package.join("b.txt");
        let logical = package.join("logical.txt");
        std::fs::write(&target_a, "old\n").unwrap();
        std::fs::write(&target_b, "old\n").unwrap();
        symlink(&target_a, &logical).unwrap();
        let mut request = request(&logical, "old", "new");
        request.call_input = Some(serde_json::json!({
            "file_path": logical.display().to_string(),
            "old_string": "old",
            "new_string": "new",
            CHECKED_EDIT_DESTINATION_KEY: target_a.display().to_string()
        }));
        std::fs::remove_file(&logical).unwrap();
        symlink(&target_b, &logical).unwrap();
        let mut context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        seed_read_state(&mut context, &logical, "old\n");
        let result = FileEditTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let ToolOutput::EditError(output) = result.data else {
            panic!("retarget must fail")
        };
        assert!(output.content.contains(FILE_UNEXPECTEDLY_MODIFIED_ERROR));
        assert!(output.dynamic_skill_dirs.is_empty());
        assert_eq!(std::fs::read_to_string(target_a).unwrap(), "old\n");
        assert_eq!(std::fs::read_to_string(target_b).unwrap(), "old\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicitly_user_updated_path_is_authoritative_and_re_pinned() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let root = temp_root("user-path-update");
        let original_path = root.join("original.txt");
        let updated_path = root.join("updated.txt");
        std::fs::write(&original_path, "old\n").unwrap();
        std::fs::write(&updated_path, "old\n").unwrap();
        let request = request(&original_path, "old", "new");
        let mut context = ToolUseContext::default();
        seed_read_state(&mut context, &original_path, "old\n");
        seed_read_state(&mut context, &updated_path, "old\n");
        let updated = serde_json::json!({
            "file_path": updated_path.display().to_string(),
            "old_string": "old",
            "new_string": "new",
            "replace_all": false
        });
        let result = crate::services::tools::tool_execution::check_permissions_and_call_tool_with_response_async(
            &request,
            &crate::types::permissions::PermissionPromptResponse::allow_once_with_input(updated),
            false,
            None,
            &context,
            None,
        )
        .await;
        assert_eq!(std::fs::read_to_string(&original_path).unwrap(), "old\n");
        assert_eq!(std::fs::read_to_string(&updated_path).unwrap(), "new\n");
        assert_eq!(result.new_context.user_modified, Some(true));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn permission_updated_input_marks_semantic_user_modification() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let root = temp_root("user-modified");
        let path = root.join("value.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut context = ToolUseContext::default();
        seed_read_state(&mut context, &path, "old\n");
        let request = request(&path, "old", "proposed");
        let updated = serde_json::json!({
            "file_path": path.display().to_string(),
            "old_string": "old",
            "new_string": "accepted   \n",
            "replace_all": false
        });
        let result = crate::services::tools::tool_execution::check_permissions_and_call_tool_with_response_async(
            &request,
            &crate::types::permissions::PermissionPromptResponse::allow_once_with_input(updated),
            false,
            None,
            &context,
            None,
        )
        .await;
        assert_eq!(result.new_context.user_modified, Some(true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "accepted   \n\n");
        let model = result.tool_result.expect("model result");
        let crate::types::message::UserContent::ToolResult(block) = &model.content[0] else {
            panic!("expected tool result block")
        };
        assert!(
            block
                .content
                .contains("user modified your proposed changes")
        );
        assert_eq!(
            block.tool_use_result.as_ref().unwrap()["userModified"],
            true
        );
        assert_eq!(
            block.tool_use_result.as_ref().unwrap()["newString"],
            "accepted   \n"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
