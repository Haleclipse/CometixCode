//! NotebookEdit tool metadata and UI.
//!
//! Maps to:
//! - CC `tools/NotebookEditTool/NotebookEditTool.ts`
//! - CC `tools/NotebookEditTool/constants.ts`
//! - CC `tools/NotebookEditTool/prompt.ts`
//! - CC `tools/NotebookEditTool/UI.tsx`
//!
//! Execution dispatch lives in `services/tools/tool_execution.rs`; the
//! execution body below mirrors the official per-tool `call`/map/render phases
//! for local `.ipynb` files.

pub mod constants;
pub mod prompt;
pub mod ui;

/// Private destination transport carried only by `PermissionRequest.call_input`
/// (`#[serde(skip)]`), never by SDK messages or transcript persistence.
pub(crate) const CHECKED_NOTEBOOK_DESTINATION_KEY: &str = "__cometix_checked_notebook_destination";
pub(crate) const CHECKED_NOTEBOOK_APPROVED_PATH_KEY: &str =
    "__cometix_checked_notebook_approved_path";

pub(crate) fn resolved_notebook_destination(path: &std::path::Path) -> std::path::PathBuf {
    let candidate = crate::utils::fs_operations::get_paths_for_permission_check(path)
        .into_iter()
        .last()
        .unwrap_or_else(|| path.to_path_buf());
    crate::utils::path::expand_path(&candidate.display().to_string(), path.parent())
        .unwrap_or(candidate)
}

fn expected_notebook_destination(
    request: &crate::types::permissions::PermissionRequest,
    context: &crate::tool::ToolUseContext,
) -> Option<std::path::PathBuf> {
    let call_input = request.call_input.as_ref()?;
    let original_path = call_input.get("notebook_path")?.as_str()?;
    let approved_path = call_input
        .get(CHECKED_NOTEBOOK_APPROVED_PATH_KEY)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(original_path);
    let approved_path = resolve_notebook_path(approved_path, &context.effective_cwd());
    let execution_path = request.input.get("notebook_path")?.as_str()?;
    let execution_path = resolve_notebook_path(execution_path, &context.effective_cwd());
    let destination = call_input
        .get(CHECKED_NOTEBOOK_DESTINATION_KEY)?
        .as_str()
        .map(std::path::PathBuf::from)?;
    // A hook-final path that differs from the approved logical path must not
    // inherit the old physical grant. Keep returning the approved destination:
    // the call-time comparison then fails unless both resolve identically.
    if execution_path != approved_path {
        return Some(destination);
    }
    Some(destination)
}

fn is_non_regular_existing_notebook(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && !metadata.is_file())
}

/// Maps to: CC `NotebookEditTool.ts:30-57` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "notebook_path",
                zod::string().describe(
                    "The absolute path to the Jupyter notebook file to edit (must be absolute, not relative)",
                ),
            ),
            (
                "cell_id",
                zod::string().optional().describe(
                    "The ID of the cell to edit. When inserting a new cell, the new cell will be inserted after the cell with this ID, or at the beginning if not specified.",
                ),
            ),
            (
                "new_source",
                zod::string().describe("The new source for the cell"),
            ),
            (
                "cell_type",
                zod::enumeration(vec!["code", "markdown"]).optional().describe(
                    "The type of the cell (code or markdown). If not specified, it defaults to the current cell type. If using edit_mode=insert, this is required.",
                ),
            ),
            (
                "edit_mode",
                zod::enumeration(vec!["replace", "insert", "delete"]).optional().describe(
                    "The type of edit to make (replace, insert, delete). Defaults to replace.",
                ),
            ),
        ])
    })
}

/// Maps to: CC `tools/NotebookEditTool/NotebookEditTool.ts:30-58` strict zod
/// `inputSchema` plus metadata at `:90-132`.
pub fn notebook_edit_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: constants::NOTEBOOK_EDIT_TOOL_NAME.to_string(),
        // ToolDefinition.description is the API schema field, so it maps to
        // CC `prompt()` (utils/api.ts), not the separate metadata description().
        description: prompt::PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// Maps to: CC `tools/NotebookEditTool/NotebookEditTool.ts:88` `export type
/// Output = z.infer<OutputSchema>` (schema at :60-85) — the single type the
/// tool yields from `call()`, records as the message's `toolUseResult`, and
/// the render path recovers via `outputSchema.safeParse` ([`ui::parse_output`]
/// is the Rust stand-in). The tool keeps its error-as-data shape: a failed
/// edit still yields this object with `error` set, and
/// `mapToolResultToToolResultBlockParam` derives `is_error` from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub new_source: String,
    pub cell_id: Option<String>,
    pub cell_type: String,
    pub language: String,
    pub edit_mode: String,
    pub error: Option<String>,
    pub notebook_path: String,
    pub original_file: String,
    pub updated_file: String,
    /// Private context-update transport; not part of CC's outputSchema and
    /// never serialized into the raw `toolUseResult`.
    pub read_timestamp_ms: Option<i64>,
}

/// Maps to: CC `tools/NotebookEditTool/NotebookEditTool.ts:295-468` `call(...)`.
/// Validation runs at the shared pre-hook gate; this function performs the
/// file read/JSON mutation/write and returns the official output shape,
/// including error-as-data behavior.
pub(crate) fn notebook_edit_output(args: &serde_json::Value) -> Output {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    notebook_edit_output_with_cwd(args, &cwd)
}

fn notebook_edit_output_with_cwd(args: &serde_json::Value, cwd: &std::path::Path) -> Output {
    notebook_edit_output_with_target(args, cwd, None, None)
}

fn notebook_edit_output_with_target(
    args: &serde_json::Value,
    cwd: &std::path::Path,
    pinned_target: Option<&std::path::Path>,
    expected_read_timestamp_ms: Option<i64>,
) -> Output {
    // Maps to the strict snake_case zod inputSchema. Compatibility aliases
    // belong to transcript/UI recovery, not the model execution boundary.
    let notebook_path = args
        .get("notebook_path")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let new_source = args
        .get("new_source")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let cell_id = args
        .get("cell_id")
        .and_then(|value| value.as_str())
        .map(ToString::to_string);
    let mut cell_type = args
        .get("cell_type")
        .and_then(|value| value.as_str())
        .map(ToString::to_string);
    let original_edit_mode = args
        .get("edit_mode")
        .and_then(|value| value.as_str())
        .map(ToString::to_string);

    let full_path = resolve_notebook_path(notebook_path, cwd);
    let full_path_display = full_path.to_string_lossy().to_string();
    let io_target = pinned_target
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| resolved_notebook_destination(&full_path));
    // Maps to CC `readFileSyncWithMetadata(fullPath)`: mutation works on
    // normalized LF content while write-back preserves encoding/line endings.
    let file_metadata = match crate::utils::file_read::read_file_sync_with_metadata(&io_target) {
        Ok(metadata) => metadata,
        Err(error) => {
            return notebook_edit_error_output(
                new_source,
                cell_type,
                cell_id,
                full_path_display,
                error.to_string(),
            );
        }
    };
    let content = file_metadata.content;
    if let Some(expected_timestamp) = expected_read_timestamp_ms {
        match crate::utils::file::get_file_modification_time_result(&io_target) {
            Ok(current_timestamp) if current_timestamp > expected_timestamp => {
                return notebook_edit_error_output(
                    new_source,
                    cell_type,
                    cell_id,
                    full_path_display,
                    "File has been unexpectedly modified. Read it again before attempting to write it.",
                );
            }
            Ok(_) => {}
            Err(error) => {
                return notebook_edit_error_output(
                    new_source,
                    cell_type,
                    cell_id,
                    full_path_display,
                    error.to_string(),
                );
            }
        }
    }

    let mut notebook = match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(notebook) => notebook,
        Err(_) => {
            return notebook_edit_error_output(
                new_source,
                cell_type,
                cell_id,
                full_path_display,
                "Notebook is not valid JSON.",
            );
        }
    };

    // CC `:379` is `notebook.metadata.language_info?.name ?? 'python'` — the
    // optional chain sits on `language_info`, NOT on `metadata`. A notebook
    // with no `metadata` key therefore throws a TypeError into the catch at
    // `:457-469` and the file is never written; only a present-but-partial
    // `metadata` falls back to 'python'.
    let Some(metadata) = notebook.get("metadata") else {
        return notebook_edit_error_output(
            new_source,
            cell_type,
            cell_id,
            full_path_display,
            "Cannot read properties of undefined (reading 'language_info')",
        );
    };
    let language = metadata
        .pointer("/language_info/name")
        .and_then(|value| value.as_str())
        .unwrap_or("python")
        .to_string();
    let nbformat = notebook
        .get("nbformat")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let nbformat_minor = notebook
        .get("nbformat_minor")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);

    // Keep `undefined` distinct from the explicit "replace" input until the
    // output projection, matching `originalEditMode` in CC.
    let mut edit_mode = original_edit_mode.clone();
    let mut output_cell_id = None;
    let mutation_result = {
        let cells = match notebook
            .get_mut("cells")
            .and_then(|value| value.as_array_mut())
        {
            Some(cells) => cells,
            None => {
                // DEVIATION(SAFETY): reachable via `edit_mode=insert` with no
                // `cell_id` (neither side's validateInput touches `cells`).
                // CC `:372` throws a raw TypeError ("Cannot read properties
                // of undefined (reading 'length')") into the catch; this port
                // returns the same stable copy the validate-side branch uses
                // (`:549-554` carries the sibling declaration).
                return notebook_edit_error_output(
                    new_source,
                    cell_type,
                    cell_id,
                    full_path_display,
                    "Notebook is not valid JSON.",
                );
            }
        };

        // CC keeps `findIndex`'s -1 when neither the cell id nor `cell-N`
        // parsing resolves; the JS splice/index rules below decide what that
        // means per edit mode.
        let cell_index: i64 = if let Some(cell_id) = cell_id.as_deref().filter(|id| !id.is_empty())
        {
            let index = find_cell_index_by_id(cells, cell_id)
                .or_else(|| parse_notebook_cell_index(cell_id).and_then(|parsed| parsed.index))
                .and_then(|index| i64::try_from(index).ok())
                .unwrap_or(-1);
            if original_edit_mode.as_deref() == Some("insert") {
                index.saturating_add(1)
            } else {
                index
            }
        } else {
            // CC inserts at the beginning when cell_id is omitted. The +1
            // adjustment lives only in the `cell_id` branch above.
            0
        };

        if edit_mode.as_deref() == Some("replace")
            && Some(cell_index) == i64::try_from(cells.len()).ok()
        {
            edit_mode = Some("insert".to_string());
            if cell_type.is_none() {
                cell_type = Some("code".to_string());
            }
        }

        // Cell IDs are only emitted for nbformat 4.5+. Older notebook formats
        // leave the output field undefined even for replace/delete.
        if nbformat > 4.0 || (nbformat == 4.0 && nbformat_minor >= 5.0) {
            if edit_mode.as_deref() == Some("insert") {
                output_cell_id = Some(generated_notebook_cell_id());
            } else {
                output_cell_id = cell_id.clone();
            }
        }

        if edit_mode.as_deref() == Some("delete") {
            let start = splice_start(cell_index, cells.len());
            // JS Array.splice is a no-op when the start is beyond the end.
            if start < cells.len() {
                cells.remove(start);
            }
            Ok(())
        } else if edit_mode.as_deref() == Some("insert") {
            let new_cell = new_notebook_cell(
                cell_type.as_deref().unwrap_or("code"),
                output_cell_id.clone(),
                &new_source,
            );
            cells.insert(splice_start(cell_index, cells.len()), new_cell);
            Ok(())
        } else if cell_index < 0 || cell_index as usize >= cells.len() {
            // CC indexes `notebook.cells[cellIndex]!` unguarded, so the model
            // sees the raw TypeError from assigning through `undefined`.
            Err("Cannot set properties of undefined (setting 'source')".to_string())
        } else {
            let cell_index = cell_index as usize;
            let target_cell = &mut cells[cell_index];
            if let Some(object) = target_cell.as_object_mut() {
                object.insert(
                    "source".to_string(),
                    serde_json::Value::String(new_source.clone()),
                );
                let is_code_cell =
                    object.get("cell_type").and_then(|value| value.as_str()) == Some("code");
                if is_code_cell {
                    object.insert("execution_count".to_string(), serde_json::Value::Null);
                    object.insert("outputs".to_string(), serde_json::Value::Array(Vec::new()));
                }
                if let Some(next_cell_type) = cell_type.as_deref() {
                    if object.get("cell_type").and_then(|value| value.as_str())
                        != Some(next_cell_type)
                    {
                        object.insert(
                            "cell_type".to_string(),
                            serde_json::Value::String(next_cell_type.to_string()),
                        );
                    }
                }
                Ok(())
            } else {
                // DEVIATION(SAFETY): a non-object cell (e.g. `cells: ["x"]`)
                // passes both sides' validateInput. CC `:419` assigns
                // `cell.source` on the primitive, which strict mode rejects
                // with "Cannot create property 'source' on string 'x'"; this
                // port names the offending index instead of reproducing the
                // engine's message.
                Err(format!(
                    "Cell with index {cell_index} is not a valid notebook cell."
                ))
            }
        }
    };

    if let Err(error) = mutation_result {
        return notebook_edit_error_output(
            new_source,
            cell_type,
            cell_id,
            full_path_display,
            error,
        );
    }

    let updated_content = match stringify_notebook(&notebook) {
        Ok(updated_content) => updated_content,
        Err(error) => {
            return notebook_edit_error_output(
                new_source,
                cell_type,
                cell_id,
                full_path_display,
                error,
            );
        }
    };
    if let Err(error) = crate::utils::file::write_text_content_to_target(
        &io_target,
        &updated_content,
        file_metadata.encoding,
        file_metadata.line_endings,
    ) {
        return notebook_edit_error_output(
            new_source,
            cell_type,
            cell_id,
            full_path_display,
            error.to_string(),
        );
    }
    let read_timestamp_ms = match crate::utils::file::get_file_modification_time_result(&io_target)
    {
        Ok(timestamp) => timestamp,
        Err(error) => {
            return notebook_edit_error_output(
                new_source,
                cell_type,
                cell_id,
                full_path_display,
                error.to_string(),
            );
        }
    };

    Output {
        new_source,
        cell_id: output_cell_id,
        cell_type: cell_type.unwrap_or_else(|| "code".to_string()),
        language,
        edit_mode: edit_mode.unwrap_or_else(|| "replace".to_string()),
        error: None,
        notebook_path: full_path_display,
        original_file: content,
        updated_file: updated_content,
        read_timestamp_ms: Some(read_timestamp_ms),
    }
}

/// Maps to Node `path.resolve(getCwd(), notebook_path)` while preserving the
/// official behavior that a relative path is accepted despite schema prose.
fn resolve_notebook_path(path: &str, cwd: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;

    let path = std::path::PathBuf::from(path);
    let candidate = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    candidate
        .components()
        .fold(std::path::PathBuf::new(), |mut normalized, component| {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                other => normalized.push(other.as_os_str()),
            }
            normalized
        })
}

/// Maps to: CC `tools/NotebookEditTool/NotebookEditTool.ts:176-293`
/// `validateInput(...)`.
fn validate_notebook_input(
    args: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> crate::tool::ValidationResult {
    let notebook_path = args
        .get("notebook_path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    // Check the raw input as well as the platform-resolved path. On POSIX,
    // PathBuf would otherwise reinterpret a Windows UNC path as cwd-relative
    // before this credential-leak guard can run.
    if notebook_path.starts_with("\\\\") || notebook_path.starts_with("//") {
        return crate::tool::ValidationResult::ok();
    }
    let full_path = resolve_notebook_path(notebook_path, &context.effective_cwd());
    let full_path_text = full_path.to_string_lossy().to_string();

    // Maps to the official UNC guard: do not touch the filesystem because a
    // metadata lookup can leak Windows credentials.
    if full_path_text.starts_with("\\\\") || full_path_text.starts_with("//") {
        return crate::tool::ValidationResult::ok();
    }
    if full_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("ipynb")
    {
        return crate::tool::ValidationResult::error(
            "File must be a Jupyter notebook (.ipynb file). For editing other file types, use the FileEdit tool.",
            2,
        );
    }

    let edit_mode = args
        .get("edit_mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("replace");
    if !matches!(edit_mode, "replace" | "insert" | "delete") {
        return crate::tool::ValidationResult::error(
            "Edit mode must be replace, insert, or delete.",
            4,
        );
    }
    let cell_type = args.get("cell_type").and_then(serde_json::Value::as_str);
    if edit_mode == "insert" && cell_type.is_none() {
        return crate::tool::ValidationResult::error(
            "Cell type is required when using edit_mode=insert.",
            5,
        );
    }

    let Some(read_timestamp_ms) = context
        .read_file_state
        .get(&full_path)
        .and_then(|entry| entry.timestamp_ms)
    else {
        return crate::tool::ValidationResult::error(
            "File has not been read yet. Read it first before writing to it.",
            9,
        );
    };
    // DEVIATION(SAFETY): CC `:230` calls `getFileModificationTime`
    // (`file.ts:66-69` = bare `statSync().mtimeMs`), so a file deleted
    // between the read-state check and here throws the raw ENOENT out of
    // validateInput — which also makes CC's own code 1 branch (`:243-248`)
    // unreachable. This port keeps the failure inside the validation result
    // (code 1, the branch CC intended) rather than propagating a raw IO
    // error; the copy differs from Node's message by construction.
    let current_mtime_ms = match crate::utils::file::get_file_modification_time_result(&full_path) {
        Ok(timestamp) => timestamp,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return crate::tool::ValidationResult::error("Notebook file does not exist.", 1);
        }
        Err(error) => {
            return crate::tool::ValidationResult::error(error.to_string(), 1);
        }
    };
    if current_mtime_ms > read_timestamp_ms {
        return crate::tool::ValidationResult::error(
            "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it.",
            10,
        );
    }

    let content = match crate::utils::file_read::read_file_sync_with_metadata(&full_path) {
        Ok(metadata) => metadata.content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return crate::tool::ValidationResult::error("Notebook file does not exist.", 1);
        }
        Err(error) => {
            return crate::tool::ValidationResult::error(error.to_string(), 1);
        }
    };
    // `safeParseJSON` strips one leading BOM; the non-memoized call path below
    // deliberately does not, matching the subtle CC validation/call split.
    let validation_content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let notebook = match serde_json::from_str::<serde_json::Value>(validation_content) {
        Ok(notebook) => notebook,
        Err(_) => {
            return crate::tool::ValidationResult::error("Notebook is not valid JSON.", 6);
        }
    };
    let Some(cells) = notebook.get("cells").and_then(serde_json::Value::as_array) else {
        // DEVIATION(SAFETY): CC lets `.cells.findIndex` throw for structurally
        // invalid but parseable JSON. Return the nearby validation error rather
        // than exposing a JavaScript-engine-specific TypeError.
        return crate::tool::ValidationResult::error("Notebook is not valid JSON.", 6);
    };
    let cell_id = args
        .get("cell_id")
        .and_then(serde_json::Value::as_str)
        .filter(|cell_id| !cell_id.is_empty());
    let Some(cell_id) = cell_id else {
        if edit_mode != "insert" {
            return crate::tool::ValidationResult::error(
                "Cell ID must be specified when not inserting a new cell.",
                7,
            );
        }
        return crate::tool::ValidationResult::ok();
    };
    if find_cell_index_by_id(cells, cell_id).is_some() {
        return crate::tool::ValidationResult::ok();
    }
    if let Some(parsed) = parse_notebook_cell_index(cell_id) {
        if parsed.index.and_then(|index| cells.get(index)).is_none() {
            return crate::tool::ValidationResult::error(
                format!(
                    "Cell with index {} does not exist in notebook.",
                    parsed.display
                ),
                7,
            );
        }
        return crate::tool::ValidationResult::ok();
    }
    crate::tool::ValidationResult::error(
        format!("Cell with ID \"{cell_id}\" not found in notebook."),
        8,
    )
}

pub(crate) fn notebook_edit_error_from_args(
    args: &serde_json::Value,
    cwd: &std::path::Path,
    error: impl Into<String>,
) -> Output {
    let notebook_path = args
        .get("notebook_path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    notebook_edit_error_output(
        args.get("new_source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        args.get("cell_type")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        args.get("cell_id")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        resolve_notebook_path(notebook_path, cwd)
            .display()
            .to_string(),
        error,
    )
}

fn notebook_edit_error_output(
    new_source: String,
    cell_type: Option<String>,
    cell_id: Option<String>,
    notebook_path: String,
    error: impl Into<String>,
) -> Output {
    Output {
        new_source,
        cell_id,
        cell_type: cell_type.unwrap_or_else(|| "code".to_string()),
        language: "python".to_string(),
        edit_mode: "replace".to_string(),
        error: Some(error.into()),
        notebook_path,
        original_file: String::new(),
        updated_file: String::new(),
        read_timestamp_ms: None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedNotebookCellIndex {
    index: Option<usize>,
    display: String,
}

/// Maps to JS `parseInt(..., 10)`: every decimal is first projected through
/// IEEE-754, including host-representable integers above `Number.MAX_SAFE_INTEGER`.
/// Values larger than the host index width still reach the official
/// out-of-bounds error rather than falling back to the "ID not found" branch.
fn parse_notebook_cell_index(cell_id: &str) -> Option<ParsedNotebookCellIndex> {
    let digits = cell_id.strip_prefix("cell-")?;
    if digits.is_empty() || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let number = digits.parse::<f64>().ok()?;
    let display = if number.is_infinite() {
        "Infinity".to_string()
    } else {
        format_ecmascript_number(number)
    };
    let index = if number.is_finite() && number >= 0.0 {
        let candidate = number as u128;
        (candidate <= usize::MAX as u128).then_some(candidate as usize)
    } else {
        None
    };
    Some(ParsedNotebookCellIndex { index, display })
}

/// Maps to: JS `Array.prototype.splice` start normalization — negatives count
/// back from the end, oversized values clamp to the length.
fn splice_start(index: i64, len: usize) -> usize {
    if index < 0 {
        usize::try_from(i64::try_from(len).unwrap_or(i64::MAX).saturating_add(index)).unwrap_or(0)
    } else {
        usize::try_from(index).unwrap_or(usize::MAX).min(len)
    }
}

fn find_cell_index_by_id(cells: &[serde_json::Value], cell_id: &str) -> Option<usize> {
    cells.iter().position(|cell| {
        cell.get("id")
            .and_then(|value| value.as_str())
            .is_some_and(|id| id == cell_id)
    })
}

fn new_notebook_cell(
    cell_type: &str,
    cell_id: Option<String>,
    new_source: &str,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "cell_type".to_string(),
        serde_json::Value::String(if cell_type == "markdown" {
            "markdown".to_string()
        } else {
            "code".to_string()
        }),
    );
    if let Some(cell_id) = cell_id {
        object.insert("id".to_string(), serde_json::Value::String(cell_id));
    }
    object.insert(
        "source".to_string(),
        serde_json::Value::String(new_source.to_string()),
    );
    object.insert(
        "metadata".to_string(),
        serde_json::Value::Object(serde_json::Map::new()),
    );
    if cell_type != "markdown" {
        object.insert("execution_count".to_string(), serde_json::Value::Null);
        object.insert("outputs".to_string(), serde_json::Value::Array(Vec::new()));
    }
    serde_json::Value::Object(object)
}

/// Maps to CC `Math.random().toString(36).substring(2, 15)`.
fn generated_notebook_cell_id() -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let mut value = u64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
    let mut output = [b'0'; 11];
    for slot in output.iter_mut().rev() {
        *slot = DIGITS[(value % 36) as usize];
        value /= 36;
    }
    String::from_utf8(output.to_vec()).unwrap_or_else(|_| "0".to_string())
}

/// Maps to `JSON.stringify(notebook, null, 1)`. serde_json deliberately
/// preserves the source insertion order, while number formatting passes
/// through IEEE-754 and ECMAScript's decimal/scientific thresholds.
fn stringify_notebook(value: &serde_json::Value) -> Result<String, String> {
    fn indent(output: &mut String, depth: usize) {
        output.extend(std::iter::repeat_n(' ', depth));
    }

    fn javascript_array_index(key: &str) -> Option<u32> {
        if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
            return None;
        }
        let index = key.parse::<u32>().ok()?;
        (index != u32::MAX && index.to_string() == key).then_some(index)
    }

    fn write_value(
        value: &serde_json::Value,
        depth: usize,
        output: &mut String,
    ) -> Result<(), String> {
        match value {
            serde_json::Value::Null => output.push_str("null"),
            serde_json::Value::Bool(value) => {
                output.push_str(if *value { "true" } else { "false" })
            }
            serde_json::Value::Number(number) => {
                let value = number
                    .as_f64()
                    .ok_or_else(|| format!("Unable to project JSON number {number} to f64"))?;
                output.push_str(&format_ecmascript_number(value));
            }
            serde_json::Value::String(value) => {
                output.push_str(&serde_json::to_string(value).map_err(|error| error.to_string())?)
            }
            serde_json::Value::Array(values) => {
                if values.is_empty() {
                    output.push_str("[]");
                } else {
                    output.push_str("[\n");
                    for (index, value) in values.iter().enumerate() {
                        indent(output, depth + 1);
                        write_value(value, depth + 1, output)?;
                        if index + 1 != values.len() {
                            output.push(',');
                        }
                        output.push('\n');
                    }
                    indent(output, depth);
                    output.push(']');
                }
            }
            serde_json::Value::Object(values) => {
                if values.is_empty() {
                    output.push_str("{}");
                } else {
                    output.push_str("{\n");
                    let mut entries = values.iter().collect::<Vec<_>>();
                    // ECMAScript own-property ordering puts array-index keys
                    // first in ascending order, then preserves string-key
                    // insertion order from JSON.parse.
                    entries.sort_by(|(left, _), (right, _)| {
                        match (javascript_array_index(left), javascript_array_index(right)) {
                            (Some(left), Some(right)) => left.cmp(&right),
                            (Some(_), None) => std::cmp::Ordering::Less,
                            (None, Some(_)) => std::cmp::Ordering::Greater,
                            (None, None) => std::cmp::Ordering::Equal,
                        }
                    });
                    for (index, (key, value)) in entries.iter().enumerate() {
                        indent(output, depth + 1);
                        output.push_str(
                            &serde_json::to_string(key).map_err(|error| error.to_string())?,
                        );
                        output.push_str(": ");
                        write_value(value, depth + 1, output)?;
                        if index + 1 != entries.len() {
                            output.push(',');
                        }
                        output.push('\n');
                    }
                    indent(output, depth);
                    output.push('}');
                }
            }
        }
        Ok(())
    }

    let mut output = String::new();
    write_value(value, 0, &mut output)?;
    Ok(output)
}

fn format_ecmascript_number(value: f64) -> String {
    // JSON.stringify maps non-finite Numbers to null; finite spelling and
    // negative zero use the ECMAScript formatter directly.
    if !value.is_finite() {
        return "null".to_string();
    }
    ryu_js::Buffer::new().format(value).to_string()
}

/// Behavioral half of CC `NotebookEditTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct NotebookEditTool;

impl crate::tool::ToolCall for NotebookEditTool {
    fn name(&self) -> &'static str {
        "NotebookEdit"
    }

    /// Maps to: CC `NotebookEditTool.ts:98-100` `async prompt() { return
    /// PROMPT }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::PROMPT.to_string()
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("edit Jupyter notebook cells (.ipynb)")
    }

    /// Maps to: CC `NotebookEditTool.ts:95-97` `description()` — returns the
    /// `DESCRIPTION` constant; the input is not read.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::DESCRIPTION.to_string()
    }

    /// Maps to: CC `NotebookEditTool.ts:101-103` `userFacingName()` —
    /// the constant `'Edit Notebook'`; the input is not read.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        crate::tools::notebook_edit_tool::ui::user_facing_name().to_string()
    }

    /// Maps to: CC `NotebookEditTool.ts:23,104` mounting
    /// `UI.tsx#getToolUseSummary`.
    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        crate::tools::notebook_edit_tool::ui::get_tool_use_summary(args)
    }

    /// Maps to: CC `NotebookEditTool.ts:105-108`
    /// `getActivityDescription(input)`.
    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            crate::tools::notebook_edit_tool::ui::get_tool_use_summary(args)
                .map(|summary| format!("Editing notebook {summary}"))
                .unwrap_or_else(|| "Editing notebook".to_string()),
        )
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    fn get_path(&self, args: &serde_json::Value) -> Option<String> {
        args.get("notebook_path")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        let path = args
            .get("notebook_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let mode = args
            .get("edit_mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("replace");
        let source = args
            .get("new_source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        format!("{path} {mode}: {source}")
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        validate_notebook_input(args, context)
    }

    /// Maps to CC `NotebookEditTool.checkPermissions(...)`.
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let raw_path = self.get_path(args).unwrap_or_default();
        let full_path = resolve_notebook_path(&raw_path, &context.effective_cwd());
        crate::utils::permissions::filesystem::check_write_permission_for_tool(
            &full_path.display().to_string(),
            args,
            &context.tool_permission_context,
            &context.effective_cwd(),
        )
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
        Box::pin(async move {
            let cwd = context.effective_cwd();
            if !crate::utils::env_utils::is_cometix_write_enabled() {
                return crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::NotebookEdit(notebook_edit_error_from_args(
                        args,
                        &cwd,
                        crate::tools::shared::write_gate::NOTEBOOK_EDIT_DISABLED_ERROR,
                    )),
                    new_messages: Vec::new(),
                };
            }

            let notebook_path = args
                .get("notebook_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let full_path = resolve_notebook_path(notebook_path, &cwd);
            let write_target = resolved_notebook_destination(&full_path);
            if expected_notebook_destination(request, context)
                .is_some_and(|expected| expected != write_target)
            {
                return crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::NotebookEdit(notebook_edit_error_from_args(
                        args,
                        &cwd,
                        "File has been unexpectedly modified. Read it again before attempting to write it.",
                    )),
                    new_messages: Vec::new(),
                };
            }
            // DEVIATION(SECURITY): CC writes through to the special file.
            if is_non_regular_existing_notebook(&write_target) {
                return crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::NotebookEdit(notebook_edit_error_from_args(
                        args,
                        &cwd,
                        "Cannot edit a non-regular file.",
                    )),
                    new_messages: Vec::new(),
                };
            }

            if crate::utils::file_history::file_history_enabled() {
                if let Some(store) = context.app_store.store.as_ref() {
                    crate::utils::file_history::file_history_track_edit(
                        store,
                        &full_path.display().to_string(),
                        // The ENVELOPE uuid — CC passes `parentMessage.uuid`
                        // (`NotebookEditTool.ts:315`), which is what
                        // `fileHistoryRewind` matches on (`fileHistory.ts:367`).
                        parent_message
                            .map(|message| message.uuid.as_str())
                            .unwrap_or_default(),
                    )
                    .await;
                }
            }

            // DEVIATION(SECURITY): history is the final await, and CC only
            // checks read-before-edit and freshness in `validateInput`.
            // Re-resolve the physical destination, then keep read, mtime
            // recheck, mutation, and write in one synchronous section so a
            // symlink swap after approval cannot redirect the write.
            let critical_target = resolved_notebook_destination(&full_path);
            if critical_target != write_target {
                return crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::NotebookEdit(notebook_edit_error_from_args(
                        args,
                        &cwd,
                        "File has been unexpectedly modified. Read it again before attempting to write it.",
                    )),
                    new_messages: Vec::new(),
                };
            }
            let Some(read_timestamp_ms) = context
                .read_file_state
                .get(&full_path)
                .and_then(|entry| entry.timestamp_ms)
            else {
                return crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::NotebookEdit(notebook_edit_error_from_args(
                        args,
                        &cwd,
                        "File has not been read yet. Read it first before writing to it.",
                    )),
                    new_messages: Vec::new(),
                };
            };

            let output = notebook_edit_output_with_target(
                args,
                &cwd,
                Some(&critical_target),
                Some(read_timestamp_ms),
            );
            if output.error.is_none() {
                // Maps to: CC `NotebookEditTool.ts:437-443` source-position
                // `readFileState.set(fullFilePath, ...)`.
                context.read_file_state.set_entry(
                    crate::utils::query_helpers::ReadFileStateEntry {
                        path: full_path.display().to_string(),
                        content: Some(output.updated_file.clone()),
                        timestamp_ms: output.read_timestamp_ms,
                        offset: None,
                        limit: None,
                        is_partial_view: false,
                        source: crate::utils::query_helpers::ReadFileStateSource::EditRefresh,
                    },
                );
            }
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::NotebookEdit(output),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/NotebookEditTool/NotebookEditTool.ts:133-174`
    /// `mapToolResultToToolResultBlockParam(...)`.
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::ToolResultStatus;
        match data {
            crate::tool::ToolOutput::NotebookEdit(output) => {
                if let Some(error) = output.error.as_ref().filter(|error| !error.is_empty()) {
                    return (error.clone(), ToolResultStatus::Error);
                }
                let cell_id = output.cell_id.as_deref().unwrap_or("undefined");
                let content = match output.edit_mode.as_str() {
                    "replace" => format!("Updated cell {cell_id} with {}", output.new_source),
                    "insert" => format!("Inserted cell {cell_id} with {}", output.new_source),
                    "delete" => format!("Deleted cell {cell_id}"),
                    _ => "Unknown edit mode".to_string(),
                };
                (content, ToolResultStatus::Success)
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (String::new(), ToolResultStatus::Error),
        }
    }

    /// NotebookEdit carries no display shape — the raw the trait
    /// projects below is what renders (`renderToolResultMessage` parses it
    /// with the tool's own output schema). A live `Output` is typed, so its
    /// projection always parses; the emit gate never fires here, unlike the
    /// cold paths that must gate on `ui::parse_output`.
    /// Maps to: CC recording NotebookEditTool's `Output` as the message's
    /// `toolUseResult` — always the full object thanks to the tool's
    /// error-as-data shape (a failed edit records the object with `error`
    /// set, not a bare string).
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::NotebookEdit(output) => Some(
                crate::tools::notebook_edit_tool::ui::output_to_value(output),
            ),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => Some(serde_json::Value::String(format!("Error: {content}"))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;
    use crate::utils::query_helpers::ReadFileStateEntry;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn update_read_state(
        context: &crate::tool::ToolUseContext,
        update: impl FnOnce(&mut ReadFileStateEntry),
    ) {
        let mut entries = context.read_file_state.snapshot();
        update(&mut entries[0]);
        context.read_file_state.replace(entries);
    }

    struct BootstrapRestore {
        session_id: String,
        original_cwd: std::path::PathBuf,
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

    fn permission_request(args: serde_json::Value) -> crate::types::permissions::PermissionRequest {
        crate::types::permissions::PermissionRequest {
            permission_result: None,
            id: "perm-notebook".to_string(),
            tool_use_id: "toolu-notebook".to_string(),
            tool_name: constants::NOTEBOOK_EDIT_TOOL_NAME.to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: args
                .get("notebook_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input: args,
            call_input: None,
            rule: crate::types::permissions::PermissionRuleValue::new(
                constants::NOTEBOOK_EDIT_TOOL_NAME,
                None,
            ),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: crate::types::permissions::PermissionMode::Default,
        }
    }

    #[test]
    fn notebook_edit_tool_schema_matches_official_input_shape() {
        let schema = notebook_edit_tool_schema();
        assert_eq!(schema.name, "NotebookEdit");
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["notebook_path", "new_source"])
        );
        assert_eq!(
            schema.input_schema["properties"]["cell_type"]["enum"],
            serde_json::json!(["code", "markdown"])
        );
        assert_eq!(
            schema.input_schema["properties"]["edit_mode"]["enum"],
            serde_json::json!(["replace", "insert", "delete"])
        );
        assert_eq!(schema.description, prompt::PROMPT);
        assert_ne!(schema.description, prompt::DESCRIPTION);
        assert_eq!(schema.strict, None);
        assert_eq!(schema.input_schema["additionalProperties"], false);
        assert_eq!(
            NotebookEditTool.search_hint(),
            Some("edit Jupyter notebook cells (.ipynb)")
        );
        assert!(NotebookEditTool.should_defer());
        assert_eq!(NotebookEditTool.max_result_size_chars(), 100_000);
        assert_eq!(
            NotebookEditTool.to_auto_classifier_input(&serde_json::json!({
                "notebook_path": "/repo/demo.ipynb",
                "new_source": "new",
                "edit_mode": "delete"
            })),
            "/repo/demo.ipynb delete: new"
        );
        // CC NotebookEditTool.ts:95-97 + :105-108.
        assert_eq!(
            NotebookEditTool.description(&serde_json::json!({})),
            prompt::DESCRIPTION
        );
        let cwd = crate::bootstrap::state::get_original_cwd();
        assert_eq!(
            NotebookEditTool.get_activity_description(&serde_json::json!({
                "notebook_path": cwd.join("nb/demo.ipynb").to_string_lossy(),
                "new_source": "x"
            })),
            Some("Editing notebook nb/demo.ipynb".to_string())
        );
        assert_eq!(
            NotebookEditTool.get_activity_description(&serde_json::json!({})),
            Some("Editing notebook".to_string())
        );
    }

    #[test]
    fn notebook_edit_validation_requires_fresh_read_state() {
        use crate::utils::query_helpers::{ReadFileStateEntry, ReadFileStateSource};

        let path = temp_notebook_path("fresh-read");
        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {},
                "cells": [{"cell_type": "code", "id": "cell-a", "source": "1"}]
            })
            .to_string(),
        )
        .unwrap();
        let args = serde_json::json!({
            "notebook_path": path.to_string_lossy(),
            "cell_id": "cell-a",
            "new_source": "2"
        });
        let tool = NotebookEditTool;
        let mut context = crate::tool::ToolUseContext::default();
        assert!(matches!(
            tool.validate_input(&args, &context),
            crate::tool::ValidationResult::Error { error_code: 9, .. }
        ));

        let timestamp_ms = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        context.read_file_state.set_entry(ReadFileStateEntry {
            path: path.display().to_string(),
            content: None,
            timestamp_ms: Some(timestamp_ms),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: ReadFileStateSource::Read,
        });
        let decoy = path.with_file_name("decoy.ipynb");
        context.read_file_state.set_entry(ReadFileStateEntry {
            path: decoy.display().to_string(),
            content: None,
            timestamp_ms: Some(timestamp_ms),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: ReadFileStateSource::Read,
        });
        assert!(tool.validate_input(&args, &context).is_ok());
        assert_eq!(
            context.read_file_state.keys(),
            vec![path.display().to_string(), decoy.display().to_string()]
        );
        assert!(context.read_file_state.delete(&decoy));

        // CC NotebookEdit only checks that readFileState has an entry; unlike
        // the hardened Edit contract, a ranged Read still satisfies this tool.
        update_read_state(&context, |entry| {
            entry.offset = Some(serde_json::json!(2));
            entry.limit = Some(serde_json::json!(1));
        });
        assert!(tool.validate_input(&args, &context).is_ok());

        update_read_state(&context, |entry| entry.timestamp_ms = Some(0));
        assert!(matches!(
            tool.validate_input(&args, &context),
            crate::tool::ValidationResult::Error { error_code: 10, .. }
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notebook_edit_validation_messages_and_codes_match_official_branches() {
        use crate::utils::query_helpers::{ReadFileStateEntry, ReadFileStateSource};

        let unc = serde_json::json!({
            "notebook_path": r"\\server\share\not-a-notebook.txt",
            "cell_id": "cell-a",
            "new_source": "new"
        });
        assert!(
            NotebookEditTool
                .validate_input(&unc, &crate::tool::ToolUseContext::default())
                .is_ok()
        );

        let path = temp_notebook_path("validation-codes");
        std::fs::write(&path, "not json").unwrap();
        let timestamp_ms = crate::utils::file::get_file_modification_time(&path);
        let mut context = crate::tool::ToolUseContext::default();
        context.read_file_state.set_entry(ReadFileStateEntry {
            path: path.display().to_string(),
            content: None,
            timestamp_ms,
            offset: None,
            limit: None,
            is_partial_view: false,
            source: ReadFileStateSource::Read,
        });
        let invalid_json = serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-a",
            "new_source": "new"
        });
        assert!(matches!(
            NotebookEditTool.validate_input(&invalid_json, &context),
            crate::tool::ValidationResult::Error {
                error_code: 6,
                ref message,
            } if message == "Notebook is not valid JSON."
        ));

        let missing_type = serde_json::json!({
            "notebook_path": path.display().to_string(),
            "new_source": "new",
            "edit_mode": "insert"
        });
        assert!(matches!(
            NotebookEditTool.validate_input(&missing_type, &context),
            crate::tool::ValidationResult::Error {
                error_code: 5,
                ref message,
            } if message == "Cell type is required when using edit_mode=insert."
        ));

        let wrong_extension = serde_json::json!({
            "notebook_path": path.with_extension("txt").display().to_string(),
            "cell_id": "cell-a",
            "new_source": "new"
        });
        assert!(matches!(
            NotebookEditTool.validate_input(&wrong_extension, &context),
            crate::tool::ValidationResult::Error {
                error_code: 2,
                ref message,
            } if message == "File must be a Jupyter notebook (.ipynb file). For editing other file types, use the FileEdit tool."
        ));

        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {},
                "cells": [{"cell_type": "code", "id": "cell-a", "source": "old"}]
            })
            .to_string(),
        )
        .unwrap();
        update_read_state(&context, |entry| {
            entry.timestamp_ms = crate::utils::file::get_file_modification_time(&path);
        });
        let oversized_index = serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-18446744073709551616",
            "new_source": "new"
        });
        assert!(matches!(
            NotebookEditTool.validate_input(&oversized_index, &context),
            crate::tool::ValidationResult::Error {
                error_code: 7,
                ref message,
            } if message == "Cell with index 18446744073709552000 does not exist in notebook."
        ));
        let above_max_safe_integer = serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-9007199254740993",
            "new_source": "new"
        });
        assert!(matches!(
            NotebookEditTool.validate_input(&above_max_safe_integer, &context),
            crate::tool::ValidationResult::Error {
                error_code: 7,
                ref message,
            } if message == "Cell with index 9007199254740992 does not exist in notebook."
        ));
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn notebook_path_resolution_keeps_root_when_parent_escapes_it() {
        assert_eq!(
            resolve_notebook_path("/../tmp/demo.ipynb", std::path::Path::new("/work/repo")),
            std::path::PathBuf::from("/tmp/demo.ipynb")
        );
    }

    #[test]
    fn notebook_edit_tool_replaces_cell_and_maps_official_result() {
        let path = temp_notebook_path("replace");
        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {"language_info": {"name": "python"}},
                "cells": [{
                    "cell_type": "code",
                    "id": "abc123",
                    "source": "print('old')",
                    "metadata": {},
                    "execution_count": 7,
                    "outputs": [{"output_type": "stream", "text": "old"}]
                }]
            })
            .to_string(),
        )
        .unwrap();

        let output = notebook_edit_output(&serde_json::json!({
            "notebook_path": path.to_string_lossy(),
            "cell_id": "abc123",
            "new_source": "print('new')",
            "edit_mode": "replace"
        }));

        assert_eq!(output.error, None);
        assert_eq!(output.cell_id.as_deref(), Some("abc123"));
        assert_eq!(output.language, "python");
        assert_eq!(output.edit_mode, "replace");
        assert!(output.original_file.contains("print('old')"));
        assert!(output.updated_file.contains("print('new')"));
        let updated_file = std::fs::read_to_string(&path).unwrap();
        assert!(updated_file.contains("\"source\": \"print('new')\""));
        assert!(updated_file.contains("\"execution_count\": null"));
        assert!(updated_file.contains("\"outputs\": []"));

        let tool = NotebookEditTool;
        let (content, status) = tool.map_tool_result_to_tool_result_block_param(
            &crate::tool::ToolOutput::NotebookEdit(output.clone()),
            "toolu_notebook",
        );
        assert_eq!(content, "Updated cell abc123 with print('new')".to_string());
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        // No NotebookEdit display shape — the trait projects the raw
        // `toolUseResult` with every schema field intact, and the display
        // records only the emit-gate decision.
        let data = crate::tool::ToolOutput::NotebookEdit(output);
        let raw = tool
            .tool_use_result(&data)
            .expect("notebook edit projects raw output");
        assert_eq!(raw["cell_id"], serde_json::json!("abc123"));
        assert_eq!(raw["new_source"], serde_json::json!("print('new')"));
        assert_eq!(raw["error"], serde_json::json!(""));
        // The live projection always satisfies the tool's own output schema,
        // so the render-time `safeParse` gate never suppresses it.
        assert!(crate::tools::notebook_edit_tool::ui::parse_output(&raw).is_some());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notebook_edit_tool_inserts_cell_and_error_maps_to_is_error() {
        let path = temp_notebook_path("insert");
        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {"language_info": {"name": "python"}},
                "cells": [{
                    "cell_type": "code",
                    "id": "first",
                    "source": "print('first')",
                    "metadata": {},
                    "execution_count": null,
                    "outputs": []
                }]
            })
            .to_string(),
        )
        .unwrap();

        let output = notebook_edit_output(&serde_json::json!({
            "notebook_path": path.to_string_lossy(),
            "cell_id": "first",
            "new_source": "# note",
            "cell_type": "markdown",
            "edit_mode": "insert"
        }));
        assert_eq!(output.error, None);
        assert_eq!(output.edit_mode, "insert");
        assert!(output.cell_id.as_deref().is_some_and(|id| !id.is_empty()));
        let tool = NotebookEditTool;
        let (content, status) = tool.map_tool_result_to_tool_result_block_param(
            &crate::tool::ToolOutput::NotebookEdit(output.clone()),
            "toolu_notebook",
        );
        assert!(content.starts_with("Inserted cell "));
        assert!(content.ends_with(" with # note"));
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);

        let invalid = notebook_edit_output(&serde_json::json!({
            "notebook_path": path.to_string_lossy(),
            "cell_id": "missing",
            "new_source": "print('nope')"
        }));
        let (content, status) = tool.map_tool_result_to_tool_result_block_param(
            &crate::tool::ToolOutput::NotebookEdit(invalid),
            "toolu_notebook_error",
        );
        // `validateInput` owns the friendly errorCode 8 copy; reaching `call`
        // with an unresolvable id leaves CC indexing `cells[-1]`.
        assert_eq!(
            content,
            "Cannot set properties of undefined (setting 'source')".to_string()
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Error);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notebook_stringify_matches_ecmascript_numbers_and_property_order() {
        assert_eq!(format_ecmascript_number(-0.0), "0");
        assert_eq!(format_ecmascript_number(f64::NAN), "null");
        assert_eq!(format_ecmascript_number(f64::INFINITY), "null");
        assert_eq!(format_ecmascript_number(f64::NEG_INFINITY), "null");

        let value: serde_json::Value = serde_json::from_str(
            r#"{"metadata":{"2":"two","1":"one","b":1.0,"a":9007199254740993,"small":0.000001,"tiny":0.0000001,"large":100000000000000000000,"huge":1e21}}"#,
        )
        .unwrap();
        assert_eq!(
            stringify_notebook(&value).unwrap(),
            "{\n \"metadata\": {\n  \"1\": \"one\",\n  \"2\": \"two\",\n  \"b\": 1,\n  \"a\": 9007199254740992,\n  \"small\": 0.000001,\n  \"tiny\": 1e-7,\n  \"large\": 100000000000000000000,\n  \"huge\": 1e+21\n }\n}"
        );
    }

    #[test]
    fn relative_notebook_path_is_resolved_against_tool_context_like_official() {
        let root = std::env::temp_dir().join(format!(
            "cometix-notebook-relative-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("demo.ipynb");
        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {},
                "cells": [{"cell_type": "code", "id": "cell-a", "source": "old", "metadata": {}, "execution_count": null, "outputs": []}]
            })
            .to_string(),
        )
        .unwrap();
        let output = notebook_edit_output_with_cwd(
            &serde_json::json!({
                "notebook_path": "./nested/../demo.ipynb",
                "cell_id": "cell-a",
                "new_source": "new"
            }),
            &root,
        );
        assert!(output.error.is_none());
        assert_eq!(output.notebook_path, path.display().to_string());
        assert!(std::fs::read_to_string(&path).unwrap().contains("new"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn insert_without_cell_id_goes_at_beginning_like_official() {
        let path = temp_notebook_path("insert-beginning");
        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {"custom": {"keep": true}},
                "custom_root": [1, 2, 3],
                "cells": [
                    {"cell_type": "code", "id": "first", "source": "first", "metadata": {"tags": ["keep"]}, "execution_count": null, "outputs": []},
                    {"cell_type": "markdown", "id": "second", "source": "second", "metadata": {}}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let output = notebook_edit_output(&serde_json::json!({
            "notebook_path": path.display().to_string(),
            "new_source": "inserted",
            "cell_type": "markdown",
            "edit_mode": "insert"
        }));
        assert!(output.error.is_none(), "error={:?}", output.error);
        let updated: serde_json::Value = serde_json::from_str(&output.updated_file).unwrap();
        assert_eq!(updated["cells"][0]["source"], "inserted");
        assert_eq!(updated["cells"][1]["id"], "first");
        assert_eq!(updated["cells"][1]["metadata"]["tags"][0], "keep");
        assert_eq!(updated["metadata"]["custom"]["keep"], true);
        assert_eq!(updated["custom_root"], serde_json::json!([1, 2, 3]));
        assert!(output
            .cell_id
            .as_deref()
            .is_some_and(|id| id.len() <= 13 && id.chars().all(|ch| ch.is_ascii_alphanumeric())));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn empty_cell_id_is_treated_as_omitted_for_insert_like_official() {
        let path = temp_notebook_path("empty-cell-id");
        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {},
                "cells": [{"cell_type": "code", "id": "first", "source": "first", "metadata": {}, "execution_count": null, "outputs": []}]
            })
            .to_string(),
        )
        .unwrap();
        let output = notebook_edit_output(&serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "",
            "new_source": "zero",
            "cell_type": "markdown",
            "edit_mode": "insert"
        }));
        assert!(output.error.is_none());
        let updated: serde_json::Value = serde_json::from_str(&output.updated_file).unwrap();
        assert_eq!(updated["cells"][0]["source"], "zero");
        assert_eq!(updated["cells"][1]["id"], "first");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn integral_float_nbformat_values_follow_javascript_number_semantics() {
        let path = temp_notebook_path("float-nbformat");
        std::fs::write(
            &path,
            r#"{"nbformat":4.0,"nbformat_minor":5.0,"metadata":{},"cells":[]}"#,
        )
        .unwrap();
        let output = notebook_edit_output(&serde_json::json!({
            "notebook_path": path.display().to_string(),
            "new_source": "new",
            "cell_type": "code",
            "edit_mode": "insert"
        }));
        assert!(output.error.is_none());
        assert!(output.cell_id.as_deref().is_some_and(|id| !id.is_empty()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pre_nbformat_4_5_does_not_emit_cell_id_like_official() {
        let path = temp_notebook_path("old-nbformat");
        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 4,
                "metadata": {},
                "cells": [{"cell_type": "code", "id": "legacy", "source": "old", "metadata": {}, "execution_count": null, "outputs": []}]
            })
            .to_string(),
        )
        .unwrap();
        let output = notebook_edit_output(&serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "legacy",
            "new_source": "new"
        }));
        assert!(output.error.is_none());
        assert_eq!(output.cell_id, None);
        let (content, status) = NotebookEditTool.map_tool_result_to_tool_result_block_param(
            &crate::tool::ToolOutput::NotebookEdit(output),
            "toolu",
        );
        assert_eq!(content, "Updated cell undefined with new");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn notebook_write_preserves_crlf_and_unix_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = temp_notebook_path("crlf-mode");
        let original = serde_json::to_string_pretty(&serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {"custom": "kept"},
            "cells": [{"cell_type": "code", "id": "cell-a", "source": "old", "metadata": {"tag": true}, "execution_count": 1, "outputs": []}]
        }))
        .unwrap()
        .replace('\n', "\r\n");
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        let output = notebook_edit_output(&serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-a",
            "new_source": "new\nline",
            "edit_mode": "replace"
        }));
        assert!(output.error.is_none());
        assert!(output.original_file.contains('\n'));
        assert!(!output.original_file.contains("\r\n"));
        assert!(output.updated_file.contains("new\\nline"));
        let raw = std::fs::read(&path).unwrap();
        assert!(raw.windows(2).any(|pair| pair == b"\r\n"));
        assert!(
            !raw.iter()
                .enumerate()
                .any(|(index, byte)| *byte == b'\n' && (index == 0 || raw[index - 1] != b'\r'))
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notebook_permission_uses_resolved_context_cwd_path() {
        let root = std::env::temp_dir().join(format!(
            "cometix-notebook-permission-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut context = crate::tool::ToolUseContext::default();
        context.cwd_override = Some(root);
        context.tool_permission_context.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Edit",
                Some("nested/**".to_string()),
            )],
        );
        let input = serde_json::json!({
            "notebook_path": "nested/demo.ipynb",
            "cell_id": "cell-a",
            "new_source": "new"
        });
        assert!(matches!(
            NotebookEditTool.check_permissions(&input, &context),
            crate::utils::permissions::permission_result::PermissionResult::Deny { .. }
        ));
        assert_eq!(
            NotebookEditTool.get_path(&input).as_deref(),
            Some("nested/demo.ipynb")
        );
    }

    #[tokio::test]
    async fn no_write_gate_fails_before_history_or_mutation() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let path = temp_notebook_path("no-write");
        let original = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [{"cell_type": "code", "id": "cell-a", "source": "old", "metadata": {}, "execution_count": null, "outputs": []}]
        })
        .to_string();
        std::fs::write(&path, &original).unwrap();
        let request = permission_request(serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-a",
            "new_source": "new"
        }));
        let result = NotebookEditTool
            .call(
                &request.input,
                &request,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;
        let crate::tool::ToolOutput::NotebookEdit(output) = result.data else {
            panic!("expected NotebookEdit error output")
        };
        assert_eq!(
            output.error.as_deref(),
            Some(crate::tools::shared::write_gate::NOTEBOOK_EDIT_DISABLED_ERROR)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn notebook_edit_call_rechecks_freshness_in_final_write_section() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let path = temp_notebook_path("call-freshness");
        let original = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [{"cell_type": "code", "id": "cell-a", "source": "old", "metadata": {}, "execution_count": null, "outputs": []}]
        })
        .to_string();
        std::fs::write(&path, &original).unwrap();
        let request = permission_request(serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-a",
            "new_source": "new"
        }));
        let mut context = crate::tool::ToolUseContext::default();
        let current_timestamp = crate::utils::file::get_file_modification_time(&path).unwrap();
        context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: path.display().to_string(),
                content: Some(original.clone()),
                timestamp_ms: Some(current_timestamp.saturating_sub(1)),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });
        let result = NotebookEditTool
            .call(&request.input, &request, &context, None, None, None)
            .await;
        let crate::tool::ToolOutput::NotebookEdit(output) = result.data else {
            panic!("expected NotebookEdit output")
        };
        assert_eq!(
            output.error.as_deref(),
            Some(
                "File has been unexpectedly modified. Read it again before attempting to write it."
            )
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn notebook_edit_tracks_history_before_mutation() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _bootstrap = BootstrapRestore::capture();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _checkpointing = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING");
        let root = std::env::temp_dir().join(format!(
            "cometix-notebook-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_dir = root.join("config");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("history.ipynb");
        let original = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [{"cell_type": "code", "id": "cell-a", "source": "old", "metadata": {}, "execution_count": null, "outputs": []}]
        })
        .to_string();
        std::fs::write(&path, &original).unwrap();
        let session_id = format!("notebook-history-{}", uuid::Uuid::new_v4().simple());
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
            Some(session_file),
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
        let mut context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
        context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: path.display().to_string(),
                content: Some(original.clone()),
                timestamp_ms: crate::utils::file::get_file_modification_time(&path),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });
        let request = permission_request(serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-a",
            "new_source": "new"
        }));
        let parent = crate::types::message::AssistantMessage {
            // The ENVELOPE uuid — CC passes `parentMessage.uuid` to
            // `fileHistoryTrackEdit` (`NotebookEditTool.ts:315`). The named
            // value used to sit on the identity block instead.
            uuid: "parent-notebook-message".to_string(),
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
        let result = NotebookEditTool
            .call(
                &request.input,
                &request,
                &context,
                None,
                Some(&parent),
                None,
            )
            .await;
        let crate::tool::ToolOutput::NotebookEdit(output) = &result.data else {
            panic!("expected NotebookEdit output")
        };
        assert!(output.error.is_none());
        let read_state = context
            .read_file_state
            .get(&path)
            .expect("NotebookEdit refreshes Read state in its owner");
        assert_eq!(
            read_state.content.as_deref(),
            Some(output.updated_file.as_str())
        );
        assert_eq!(read_state.timestamp_ms, output.read_timestamp_ms);
        let state = store.get();
        let backup = state.file_history.snapshots[0]
            .tracked_file_backups
            .get("history.ipynb")
            .expect("NotebookEdit tracks original notebook");
        let backup_path = config_dir
            .join("file-history")
            .join(session_id)
            .join(backup.backup_file_name.as_deref().unwrap());
        assert_eq!(std::fs::read_to_string(backup_path).unwrap(), original);
        assert!(std::fs::read_to_string(&path).unwrap().contains("new"));
        crate::utils::session_storage::flush_session_storage()
            .await
            .unwrap();
        crate::utils::session_storage::reset_session_file_pointer();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Maps to: CC `NotebookEditTool.ts:379` — the optional chain sits on
    /// `language_info`, not on `metadata`, so a notebook with no `metadata`
    /// key throws into the catch (`:457-469`) and the file stays untouched.
    /// A present-but-partial `metadata` still falls back to 'python'.
    #[tokio::test]
    async fn notebook_edit_missing_metadata_fails_like_official_without_writing() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

        let run = |body: serde_json::Value| async move {
            let path = temp_notebook_path("metadata");
            let original = body.to_string();
            std::fs::write(&path, &original).unwrap();
            let request = permission_request(serde_json::json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "new"
            }));
            let context = crate::tool::ToolUseContext::default();
            context
                .read_file_state
                .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                    path: path.display().to_string(),
                    content: Some(original.clone()),
                    timestamp_ms: crate::utils::file::get_file_modification_time(&path),
                    offset: None,
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Read,
                });
            let result = NotebookEditTool
                .call(&request.input, &request, &context, None, None, None)
                .await;
            let crate::tool::ToolOutput::NotebookEdit(output) = result.data else {
                panic!("expected NotebookEdit output")
            };
            let on_disk = std::fs::read_to_string(&path).unwrap();
            let _ = std::fs::remove_file(&path);
            (output, on_disk, original)
        };

        let cell = serde_json::json!({
            "cell_type": "code", "id": "cell-a", "source": "old",
            "metadata": {}, "execution_count": null, "outputs": []
        });
        let (output, on_disk, original) = run(serde_json::json!({
            "nbformat": 4, "nbformat_minor": 5, "cells": [cell.clone()]
        }))
        .await;
        assert_eq!(
            output.error.as_deref(),
            Some("Cannot read properties of undefined (reading 'language_info')")
        );
        assert_eq!(on_disk, original, "the notebook must stay untouched");

        // Present but partial metadata keeps the 'python' fallback.
        let (output, on_disk, original) = run(serde_json::json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [cell]
        }))
        .await;
        assert!(output.error.is_none(), "error={:?}", output.error);
        assert_eq!(output.language, "python");
        assert_ne!(on_disk, original, "a successful edit rewrites the file");
    }

    fn temp_notebook_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cometix-notebook-edit-{name}-{}.ipynb",
            uuid::Uuid::new_v4()
        ))
    }
}
