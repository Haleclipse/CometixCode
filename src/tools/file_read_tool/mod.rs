//! Read tool metadata and execution.
//!
//! Maps to CC `tools/FileReadTool/FileReadTool.ts`. The sibling `limits`,
//! `prompt`, and `ui` modules own their corresponding CC files.

pub mod limits;
pub mod prompt;
pub mod ui;

/// Maps to: CC `FileReadTool.ts:227-243` `inputSchema` — the carrier schema,
/// one definition behind validation and the JSON Schema projection. Built once
/// per process, matching CC's `lazySchema()` one-instance-per-session guarantee.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::semantic_number::semantic_number;
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "file_path",
                zod::string().describe("The absolute path to the file to read"),
            ),
            (
                "offset",
                semantic_number(zod::number().int().nonnegative().optional()).describe(
                    "The line number to start reading from. Only provide if the file is too large to read at once",
                ),
            ),
            (
                "limit",
                semantic_number(zod::number().int().positive().optional()).describe(
                    "The number of lines to read. Only provide if the file is too large to read at once.",
                ),
            ),
            (
                "pages",
                zod::string().optional().describe(format!(
                    "Page range for PDF files (e.g., \"1-5\", \"3\", \"10-20\"). Only applicable to PDF files. Maximum {} pages per request.",
                    crate::constants::api_limits::PDF_MAX_PAGES_PER_READ,
                )),
            ),
        ])
    })
}

use crate::utils::format::format_file_size;
use crate::utils::image_resizer::{
    ImageDimensions, compress_image_buffer_with_token_limit, create_image_metadata_text,
    maybe_resize_and_downsample_image_buffer,
};
use crate::utils::notebook::{NotebookCellSource, map_notebook_cells_to_tool_result};
use base64::Engine as _;
use limits::{FileReadingLimits, get_default_file_reading_limits};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

/// Maps to: CC `IMAGE_EXTENSIONS`.
static IMAGE_EXTENSIONS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["png", "jpg", "jpeg", "gif", "webp"]));
/// Maps to: CC `tools/FileReadTool/FileReadTool.ts:162` `FileReadListener`.
/// Recoverable Rust callbacks return an opaque error; panic/non-`Error` throws
/// remain the explicit partial seams recorded by the approved L1 contract.
pub type FileReadListener = Arc<dyn Fn(&str, &str) -> anyhow::Result<()> + Send + Sync + 'static>;
static FILE_READ_LISTENERS: LazyLock<Mutex<Vec<FileReadListener>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Maps to: CC `tools/FileReadTool/FileReadTool.ts:165-173`
/// `registerFileReadListener`.
pub fn register_file_read_listener(
    listener: FileReadListener,
) -> impl Fn() + Send + Sync + 'static {
    FILE_READ_LISTENERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Arc::clone(&listener));
    move || {
        let mut listeners = FILE_READ_LISTENERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = listeners
            .iter()
            .position(|candidate| Arc::ptr_eq(candidate, &listener))
        {
            listeners.remove(index);
        }
    }
}

/// Maps to CC `FileReadTool.ts:722-725` `CYBER_RISK_MITIGATION_REMINDER`.
pub(crate) const CYBER_RISK_MITIGATION_REMINDER: &str = "\n\n<system-reminder>\nWhenever you read a file, you should consider whether it would be considered malware. You CAN and SHOULD provide analysis of malware, what it is doing. But you MUST refuse to improve or augment the code. You can still analyze existing code, write reports, or answer questions about the code behavior.\n</system-reminder>\n";

/// Device files that would hang (infinite output or blocking input).
/// Maps to: CC `BLOCKED_DEVICE_PATHS` / `isBlockedDevicePath`.
fn is_blocked_device_path(file_path: &str) -> bool {
    const BLOCKED: &[&str] = &[
        "/dev/zero",
        "/dev/random",
        "/dev/urandom",
        "/dev/full",
        "/dev/stdin",
        "/dev/tty",
        "/dev/console",
        "/dev/stdout",
        "/dev/stderr",
        "/dev/fd/0",
        "/dev/fd/1",
        "/dev/fd/2",
    ];
    if BLOCKED.contains(&file_path) {
        return true;
    }
    file_path.starts_with("/proc/")
        && (file_path.ends_with("/fd/0")
            || file_path.ends_with("/fd/1")
            || file_path.ends_with("/fd/2"))
}

/// Maps to: CC `FileReadTool.ts:347-360` `async prompt()` — computed per
/// call from `getDefaultFileReadingLimits()` (max-size instruction, targeted
/// offset nudge) and the line-format/PDF instruction picks. Single owner
/// shared by the wire schema and `ToolCall::prompt`.
pub(crate) fn api_prompt() -> String {
    let limits = get_default_file_reading_limits();
    let max_size_instruction = if limits.include_max_size_in_prompt.unwrap_or(false) {
        format!(
            ". Files larger than {} will return an error; use offset and limit for larger files",
            format_file_size(limits.max_size_bytes)
        )
    } else {
        String::new()
    };
    let offset_instruction = if limits.targeted_range_nudge.unwrap_or(false) {
        prompt::OFFSET_INSTRUCTION_TARGETED
    } else {
        prompt::OFFSET_INSTRUCTION_DEFAULT
    };
    prompt::render_prompt_template(
        prompt::LINE_FORMAT_INSTRUCTION,
        &max_size_instruction,
        offset_instruction,
        crate::utils::pdf_utils::is_pdf_supported(),
    )
}

/// Maps to CC `FileReadTool.inputSchema`.
pub fn file_read_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::FILE_READ_TOOL_NAME.to_string(),
        description: api_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        strict: Some(true),
        ..Default::default()
    }
}

/// Maps to: CC `tools/FileReadTool/FileReadTool.ts:248-332` outputSchema
/// `text` variant. No Rust carrier fields are embedded in this schema shape.
#[derive(Clone, Debug)]
pub(crate) struct ReadTextOutput {
    pub(crate) file_path: String,
    pub(crate) content: String,
    pub(crate) num_lines: usize,
    /// A final hook replacement reaches `call` without schema revalidation.
    /// Keep the JavaScript value losslessly; the strict success-output parser
    /// decides later whether terminal UI may render it.
    pub(crate) start_line: serde_json::Value,
    pub(crate) total_lines: usize,
}

/// Maps to: CC `FileReadTool.ts:270-301` outputSchema `image` variant.
#[derive(Clone, Debug)]
pub(crate) struct ReadImageOutput {
    pub(crate) base64: String,
    pub(crate) media_type: String,
    pub(crate) original_size: usize,
    pub(crate) dimensions: Option<ImageDimensions>,
}

/// Maps to: CC `FileReadTool.ts:303-309` outputSchema `notebook` variant.
#[derive(Clone, Debug)]
pub(crate) struct ReadNotebookOutput {
    pub(crate) file_path: String,
    pub(crate) cells: Vec<NotebookCellSource>,
}

/// Maps to: CC `FileReadTool.ts:326-331` outputSchema `file_unchanged` variant.
#[derive(Clone, Debug)]
pub(crate) struct ReadFileUnchangedOutput {
    pub(crate) file_path: String,
}

/// Maps to: CC `FileReadTool.ts:311-318` outputSchema `pdf` variant.
#[derive(Clone, Debug)]
pub(crate) struct ReadPdfOutput {
    pub(crate) file_path: String,
    pub(crate) base64: String,
    pub(crate) original_size: u64,
}

/// Maps to: CC `FileReadTool.ts:320-325` outputSchema `parts` variant.
#[derive(Clone, Debug)]
pub(crate) struct ReadPdfPartsOutput {
    pub(crate) file_path: String,
    pub(crate) original_size: u64,
    pub(crate) count: usize,
    pub(crate) output_dir: std::path::PathBuf,
}

/// Direct Rust discriminant projection of CC `FileReadTool.Output`'s six
/// successful variants (`FileReadTool.ts:248-335`).
#[derive(Clone, Debug)]
pub(crate) enum ReadOutput {
    Text(ReadTextOutput),
    Image(ReadImageOutput),
    Notebook(ReadNotebookOutput),
    Pdf(ReadPdfOutput),
    Parts(ReadPdfPartsOutput),
    FileUnchanged(ReadFileUnchangedOutput),
}

// Mechanical serde_json carriers for JavaScript operators written inline in
// CC `FileReadTool.ts:496-568,652-717,804-1086`. They remain private to the
// defining Read owner and carry no policy beyond String/truthiness. ECMAScript
// Number stringification delegates directly to `ryu-js` at each conversion.
fn javascript_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value
            .as_f64()
            .map(|value| ryu_js::Buffer::new().format(value).to_string())
            .unwrap_or_else(|| value.to_string()),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| match value {
                serde_json::Value::Null => String::new(),
                other => javascript_to_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        serde_json::Value::Object(_) => "[object Object]".to_string(),
    }
}

fn javascript_string_to_number(value: &str) -> f64 {
    let value = value.trim_matches(|character: char| {
        matches!(
            character,
            '\u{0009}'
                | '\u{000a}'
                | '\u{000b}'
                | '\u{000c}'
                | '\u{000d}'
                | '\u{0020}'
                | '\u{00a0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    });
    if value.is_empty() {
        return 0.0;
    }
    match value {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }

    for (lower, upper, radix) in [("0x", "0X", 16_u32), ("0b", "0B", 2), ("0o", "0O", 8)] {
        if let Some(digits) = value
            .strip_prefix(lower)
            .or_else(|| value.strip_prefix(upper))
        {
            if digits.is_empty() {
                return f64::NAN;
            }
            let mut number = 0.0;
            for digit in digits.chars() {
                let Some(digit) = digit.to_digit(radix) else {
                    return f64::NAN;
                };
                number = number * f64::from(radix) + f64::from(digit);
            }
            return number;
        }
    }

    let bytes = value.as_bytes();
    let mut index = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));
    let integer_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    let mut has_digit = index > integer_start;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fraction_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        has_digit |= index > fraction_start;
    }
    if !has_digit {
        return f64::NAN;
    }
    if matches!(bytes.get(index), Some(b'e') | Some(b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+') | Some(b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == exponent_start {
            return f64::NAN;
        }
    }
    if index != bytes.len() {
        return f64::NAN;
    }
    value.parse::<f64>().unwrap_or(f64::NAN)
}

fn javascript_to_number(value: &serde_json::Value) -> f64 {
    match value {
        serde_json::Value::Null => 0.0,
        serde_json::Value::Bool(value) => f64::from(u8::from(*value)),
        serde_json::Value::Number(value) => value.as_f64().unwrap_or(f64::NAN),
        serde_json::Value::String(value) => javascript_string_to_number(value),
        serde_json::Value::Array(_) => javascript_string_to_number(&javascript_to_string(value)),
        serde_json::Value::Object(_) => f64::NAN,
    }
}

fn javascript_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::Number(value) => value
            .as_f64()
            .is_some_and(|value| value != 0.0 && !value.is_nan()),
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => true,
    }
}

pub(crate) fn javascript_runtime_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Number(number) => number.as_f64().map_or_else(
            || value.clone(),
            |number| {
                let rendered = ryu_js::Buffer::new().format(number).to_string();
                serde_json::from_str(&rendered).unwrap_or_else(|_| value.clone())
            },
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(javascript_runtime_value).collect())
        }
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), javascript_runtime_value(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn javascript_strict_equal(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    match (left, right) {
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        (serde_json::Value::Bool(left), serde_json::Value::Bool(right)) => left == right,
        (serde_json::Value::Number(left), serde_json::Value::Number(right)) => left
            .as_f64()
            .zip(right.as_f64())
            .is_some_and(|(left, right)| left == right),
        (serde_json::Value::String(left), serde_json::Value::String(right)) => left == right,
        // JSON hook payloads and cache snapshots are independently parsed/
        // cloned objects. JavaScript reference equality is therefore false
        // even when their object/array contents are value-equal.
        (serde_json::Value::Array(_), serde_json::Value::Array(_))
        | (serde_json::Value::Object(_), serde_json::Value::Object(_)) => false,
        _ => false,
    }
}

/// Maps to: CC `FileReadTool.ts:175-185` `MaxFileReadTokenExceededError`.
#[derive(Clone, Debug, PartialEq)]
pub struct MaxFileReadTokenExceededError {
    pub token_count: usize,
    pub max_tokens: f64,
}

impl std::fmt::Display for MaxFileReadTokenExceededError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "File content ({} tokens) exceeds maximum allowed tokens ({}). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file.",
            self.token_count,
            ryu_js::Buffer::new().format(self.max_tokens)
        )
    }
}

impl std::error::Error for MaxFileReadTokenExceededError {}

/// L1 (`FileReadTool fallible source-position state carrier`): successful CC
/// `{ data, newMessages? }` ≙ one Read-owned Rust success envelope.
///
/// `memory_file_mtime_ms` maps to CC's `memoryFileMtimes` side-channel
/// (`FileReadTool.ts:741-747`): a module-level `WeakMap<object, number>`
/// keyed by the `data` object identity, carrying the auto-memory file mtime
/// from `call()` to `mapToolResultToToolResultBlockParam` without adding a
/// presentation-only field to the output schema. Rust has no object-identity
/// map, so the value rides the result envelope itself — private and
/// non-serialized, like the WeakMap entry.
#[derive(Debug)]
pub(crate) struct FileReadToolResult {
    pub(crate) data: ReadOutput,
    pub(crate) new_messages: Vec<crate::types::message::Message>,
    memory_file_mtime_ms: Option<f64>,
}

impl FileReadToolResult {
    /// Maps to: CC `memoryFileMtimes.get(data)`
    /// (`memoryFileFreshnessPrefix`, `FileReadTool.ts:749-753`).
    pub(crate) fn memory_file_mtime_ms(&self) -> Option<f64> {
        self.memory_file_mtime_ms
    }
}

/// L1 (`FileReadTool fallible source-position state carrier`): CC throw ≙ a
/// lossless Rust transport envelope retaining the original anyhow error chain.
pub(crate) struct FileReadCallFailure {
    source: anyhow::Error,
}

impl FileReadCallFailure {
    fn new(source: anyhow::Error) -> Self {
        Self { source }
    }

    pub(crate) fn message(&self) -> String {
        self.source.to_string()
    }

    fn context(self, message: String) -> Self {
        Self::new(self.source.context(message))
    }

    pub(crate) fn is_max_token_exceeded(&self) -> bool {
        self.source
            .downcast_ref::<MaxFileReadTokenExceededError>()
            .is_some()
    }

    pub(crate) fn is_file_too_large(&self) -> bool {
        self.source.chain().any(|cause| {
            cause
                .downcast_ref::<crate::utils::read_file_in_range::FileTooLargeError>()
                .is_some()
        })
    }

    pub(crate) fn is_enoent(&self) -> bool {
        crate::utils::errors::is_enoent(&self.source)
    }

    #[cfg(test)]
    fn downcast_ref<E: std::error::Error + Send + Sync + 'static>(&self) -> Option<&E> {
        self.source.downcast_ref::<E>()
    }
}

impl std::fmt::Debug for FileReadCallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("FileReadCallFailure")
            .field(&self.source.to_string())
            .finish()
    }
}

/// L1 (`FileReadTool fallible source-position state carrier`): one Read-only
/// registry envelope under the unchanged repository-wide `ToolCall::call`.
pub(crate) enum FileReadRegistryOutcome {
    Success(FileReadToolResult),
    Failure(FileReadCallFailure),
}

/// Maps to: CC `tools/FileReadTool/FileReadTool.ts:652-717`
/// `mapToolResultToToolResultBlockParam` return value, projected into the
/// repository's split text/multimodal Rust carriers.
pub(crate) struct FileReadMappedToolResult {
    pub(crate) content: String,
    pub(crate) content_blocks: Vec<crate::types::message::ToolResultContentBlock>,
}

/// L1 (`FileReadTool fallible source-position state carrier`): CC independently
/// shallow-shared synchronous field operations ≙ Arc-shared short critical
/// sections. This private sink owns only source-position scheduling.
struct FileReadSourcePositionSink<'a> {
    context: &'a crate::tool::ToolUseContext,
}

/// L1 (`FileReadTool fallible source-position state carrier`): CC `callInner`
/// awaits native I/O before token validation and source-position commits;
/// Rust's blocking worker returns only this private branch data to the same
/// source-named async owner. It commits no state and exposes no serialized
/// result or supplemental message.
#[derive(Debug)]
enum PreparedReadOutput {
    Text {
        output: ReadTextOutput,
        resolved_file_path: std::path::PathBuf,
        nested_path: String,
        cache_entry: crate::utils::query_helpers::ReadFileStateEntry,
        memory_mtime_ms: Option<f64>,
    },
    Image {
        output: ReadImageOutput,
        metadata_text: Option<String>,
        nested_path: String,
    },
    Notebook {
        output: ReadNotebookOutput,
        cache_entry: crate::utils::query_helpers::ReadFileStateEntry,
        nested_path: String,
    },
    Pdf {
        output: ReadPdfOutput,
    },
    Parts {
        output: ReadPdfPartsOutput,
        page_paths: Vec<std::path::PathBuf>,
    },
}

impl ReadOutput {
    /// Read-owned raw output-schema projection used by hooks, attachments, and
    /// the persisted `toolUseResult`. Generic execution never switches on
    /// modalities.
    pub(crate) fn to_output_schema_value(&self) -> serde_json::Value {
        match self {
            Self::Text(output) => serde_json::json!({
                "type": "text",
                "file": {
                    "filePath": output.file_path,
                    "content": output.content,
                    "numLines": output.num_lines,
                    "startLine": output.start_line,
                    "totalLines": output.total_lines,
                }
            }),
            Self::Image(output) => {
                let mut file = serde_json::json!({
                    "base64": output.base64,
                    "type": output.media_type,
                    "originalSize": output.original_size,
                });
                if let Some(dimensions) = &output.dimensions {
                    file["dimensions"] = serde_json::json!(dimensions);
                }
                serde_json::json!({"type": "image", "file": file})
            }
            Self::Notebook(output) => serde_json::json!({
                "type": "notebook",
                "file": {"filePath": output.file_path, "cells": output.cells},
            }),
            Self::Pdf(output) => serde_json::json!({
                "type": "pdf",
                "file": {
                    "filePath": output.file_path,
                    "base64": output.base64,
                    "originalSize": output.original_size,
                }
            }),
            Self::Parts(output) => serde_json::json!({
                "type": "parts",
                "file": {
                    "filePath": output.file_path,
                    "originalSize": output.original_size,
                    "count": output.count,
                    "outputDir": output.output_dir,
                }
            }),
            Self::FileUnchanged(output) => serde_json::json!({
                "type": "file_unchanged",
                "file": {"filePath": output.file_path},
            }),
        }
    }
}

impl FileReadToolResult {
    pub(crate) fn raw_output(&self) -> serde_json::Value {
        self.data.to_output_schema_value()
    }
}

impl<'a> FileReadSourcePositionSink<'a> {
    /// L1 source-position projection of CC `readFileState.get(fullFilePath)`
    /// (`FileReadTool.ts:541-568`). Cache policy stays in `FileStateCache`.
    fn cache_get(
        &self,
        path: &std::path::Path,
    ) -> Option<crate::utils::query_helpers::ReadFileStateEntry> {
        self.context.read_file_state.get(path)
    }

    /// L1 source-position projection of the ordered Set insertions at
    /// `FileReadTool.ts:579-591`; no discovery/loading policy lives here.
    fn add_dynamic_skill_dirs(&self, directories: &[std::path::PathBuf]) {
        let Some(triggers) = &self.context.dynamic_skill_dir_triggers else {
            return;
        };
        let directories = directories
            .iter()
            .map(|directory| directory.display().to_string())
            .collect::<Vec<_>>();
        crate::tool::FileReadSourceTurn::run(|| {
            triggers.with_set_in_source_turn(|set| set.extend(directories));
        });
    }
}

/// Maps to: CC `tools/FileReadTool/FileReadTool.ts:333-335` `export type
/// Output = z.infer<OutputSchema>` (schema at :247-329) — the single type the
/// tool yields from `call()`, records as the message's `toolUseResult`, and
/// the render path recovers via `outputSchema.safeParse` ([`parse_output`] is
/// the Rust stand-in). Variant/field names map one-to-one to the schema.
///
/// Serde derives mirror the wire form `{type, file: {...}}` (adjacent tag =
/// `type`, content = `file`) so this type serves the typed
/// [`crate::types::message::Attachment`] union exactly as CC's
/// `import { type Output as FileReadToolOutput }` does
/// (utils/attachments.ts:15,293-331,458-460). The Read tool's own recovery
/// seam keeps the stricter hand-written [`parse_output`] / [`output_to_value`]
/// projections (JS-runtime coercions, media-type whitelist); the serde derive
/// is the attachment-path shape, which CC never runtime-validates.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "file", rename_all = "snake_case")]
pub enum Output {
    #[serde(rename_all = "camelCase")]
    Text {
        file_path: String,
        content: String,
        /// Zod `number()` remains unrestricted for cold JSONL fidelity.
        num_lines: serde_json::Number,
        start_line: serde_json::Number,
        total_lines: serde_json::Number,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        base64: String,
        /// CC wire key is `type` (the image MIME type).
        #[serde(rename = "type")]
        media_type: String,
        original_size: serde_json::Number,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimensions: Option<OutputImageDimensions>,
    },
    #[serde(rename_all = "camelCase")]
    Notebook {
        file_path: String,
        /// Output schema is `z.array(z.any())`; preserve arbitrary cold JSONL
        /// cells even when they are not live notebook cell projections.
        cells: Vec<serde_json::Value>,
    },
    #[serde(rename_all = "camelCase")]
    Pdf {
        file_path: String,
        base64: String,
        original_size: serde_json::Number,
    },
    #[serde(rename_all = "camelCase")]
    Parts {
        file_path: String,
        original_size: serde_json::Number,
        count: serde_json::Number,
        output_dir: String,
    },
    #[serde(rename_all = "camelCase")]
    FileUnchanged { file_path: String },
}

/// Maps to: CC `ImageDimensions` (`utils/imageResizer.ts`, imported at
/// `FileReadTool.ts:47`) — the type behind the output schema's inline image
/// `dimensions` object (`FileReadTool.ts:276-283`), as the **wire projection**:
/// unrestricted Zod `number()` values and absent-not-null optionals for cold
/// JSONL fidelity. The live resize pipeline keeps the integral
/// [`crate::utils::image_resizer::ImageDimensions`]; folding the two into one
/// type needs a number-domain audit first (u32 would reject legal cold JSONL).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputImageDimensions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_width: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_height: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_width: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_height: Option<serde_json::Number>,
}

fn required_output_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    object.get(key)?.as_str().map(str::to_string)
}

fn required_output_number(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<serde_json::Number> {
    javascript_runtime_value(object.get(key)?)
        .as_number()
        .cloned()
}

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): validates the
/// `tools/FileReadTool/FileReadTool.ts:248-335` `outputSchema` contract with
/// Zod's unknown-key stripping, used by live and cold UI.
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<Output> {
    let output = value.as_object()?;
    let output_type = output.get("type")?.as_str()?;
    let file = output.get("file")?.as_object()?;
    Some(match output_type {
        "text" => Output::Text {
            file_path: required_output_string(file, "filePath")?,
            content: required_output_string(file, "content")?,
            num_lines: required_output_number(file, "numLines")?,
            start_line: required_output_number(file, "startLine")?,
            total_lines: required_output_number(file, "totalLines")?,
        },
        "image" => {
            let media_type = required_output_string(file, "type")?;
            if !matches!(
                media_type.as_str(),
                "image/jpeg" | "image/png" | "image/gif" | "image/webp"
            ) {
                return None;
            }
            let dimensions = match file.get("dimensions") {
                None => None,
                Some(value) => {
                    let object = value.as_object()?;
                    let parse = |key: &str| -> Option<Option<serde_json::Number>> {
                        match object.get(key) {
                            None => Some(None),
                            Some(value) => Some(Some(value.as_number()?.clone())),
                        }
                    };
                    Some(OutputImageDimensions {
                        original_width: parse("originalWidth")?,
                        original_height: parse("originalHeight")?,
                        display_width: parse("displayWidth")?,
                        display_height: parse("displayHeight")?,
                    })
                }
            };
            Output::Image {
                base64: required_output_string(file, "base64")?,
                media_type,
                original_size: required_output_number(file, "originalSize")?,
                dimensions,
            }
        }
        "notebook" => Output::Notebook {
            file_path: required_output_string(file, "filePath")?,
            cells: file.get("cells")?.as_array()?.clone(),
        },
        "pdf" => Output::Pdf {
            file_path: required_output_string(file, "filePath")?,
            base64: required_output_string(file, "base64")?,
            original_size: required_output_number(file, "originalSize")?,
        },
        "parts" => Output::Parts {
            file_path: required_output_string(file, "filePath")?,
            original_size: required_output_number(file, "originalSize")?,
            count: required_output_number(file, "count")?,
            output_dir: required_output_string(file, "outputDir")?,
        },
        "file_unchanged" => Output::FileUnchanged {
            file_path: required_output_string(file, "filePath")?,
        },
        _ => return None,
    })
}

/// Destructured arguments of CC `FileReadTool.call` at
/// `tools/FileReadTool/FileReadTool.ts:496-501`.
///
/// Initial model input has already passed `inputSchema`; an authoritative
/// post-hook object reaches this projection without another schema parse.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileReadInput {
    file_path: String,
    /// Destructuring defaults only `undefined`; JSON `null`, booleans,
    /// strings, arrays, fractions, and negatives remain authoritative.
    offset: serde_json::Value,
    /// `None` represents JavaScript `undefined` (field absent).
    limit: Option<serde_json::Value>,
    /// `None` represents JavaScript `undefined` (field absent).
    pages: Option<serde_json::Value>,
}

impl FileReadInput {
    /// Maps to CC `tools/FileReadTool/FileReadTool.ts:496-501` `call`
    /// parameter destructuring, including absent-only defaults and raw values.
    /// Deviation (L2, active Read Slice 4 contract): a raw non-string
    /// `file_path` returns `Invalid Read tool input` instead of Node's native
    /// `path.extname` copy; both fail at the same pre-I/O call boundary.
    fn from_args(args: &serde_json::Value) -> Result<Self, String> {
        let object = args
            .as_object()
            .ok_or_else(|| "Cannot destructure Read input".to_string())?;
        // Deliberate task contract: the Rust execution boundary retains this
        // established exact error for a raw non-string replacement while still
        // failing at the source-equivalent pre-I/O call boundary.
        let file_path = object
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Invalid Read tool input".to_string())?
            .to_string();
        Ok(Self {
            file_path,
            offset: object
                .get("offset")
                .map(javascript_runtime_value)
                .unwrap_or_else(|| serde_json::json!(1)),
            limit: object.get("limit").map(javascript_runtime_value),
            pages: object.get("pages").map(javascript_runtime_value),
        })
    }
}

/// Maps to: CC `tools/FileReadTool/FileReadTool.ts:147-159`
/// `getAlternateScreenshotPath`.
fn get_alternate_screenshot_path(file_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let filename = file_path.file_name()?.to_str()?;
    for (current, alternate) in [
        (" AM.png", "\u{202f}AM.png"),
        (" PM.png", "\u{202f}PM.png"),
        ("\u{202f}AM.png", " AM.png"),
        ("\u{202f}PM.png", " PM.png"),
    ] {
        let Some(prefix) = filename.strip_suffix(current) else {
            continue;
        };
        if prefix.is_empty() {
            return None;
        }
        return Some(file_path.with_file_name(format!("{prefix}{alternate}")));
    }
    None
}

/// Maps to: CC `FileReadTool.ts:755-772` `validateContentTokens`.
async fn validate_content_tokens(
    content: &str,
    ext: &str,
    max_tokens: f64,
) -> Result<(), FileReadCallFailure> {
    let estimate =
        crate::services::token_estimation::rough_token_count_estimation_for_file_type(content, ext);
    if estimate == 0 || estimate as f64 <= max_tokens / 4.0 {
        return Ok(());
    }
    let effective_count = crate::services::token_estimation::count_tokens_with_api(content)
        .await
        .unwrap_or(estimate);
    if effective_count as f64 > max_tokens {
        Err(FileReadCallFailure::new(anyhow::Error::new(
            MaxFileReadTokenExceededError {
                token_count: effective_count,
                max_tokens,
            },
        )))
    } else {
        Ok(())
    }
}

/// Maps to: CC `tools/FileReadTool/FileReadTool.ts:1097-1183`
/// `readImageWithTokenBudget`.
pub(crate) fn read_image_with_token_budget(
    full_file_path: &std::path::Path,
    max_tokens: f64,
    max_bytes: Option<f64>,
) -> anyhow::Result<ReadImageOutput> {
    let image_buffer = crate::utils::fs_operations::read_file_bytes(full_file_path, max_bytes)?;
    let original_size = image_buffer.len();
    if original_size == 0 {
        return Err(anyhow::anyhow!(
            "Image file is empty: {}",
            full_file_path.display()
        ));
    }

    let detected_media_type =
        crate::utils::image_resizer::detect_image_format_from_buffer(&image_buffer);
    let detected_format = detected_media_type.strip_prefix("image/").unwrap_or("png");
    // CC rethrows `ImageResizeError` and degrades to the untouched buffer for
    // anything else thrown in this block (`FileReadTool.ts:1118-1134`). The
    // Rust resizer only produces `ImageResizeError`, so the degrade arm is a
    // structural mirror rather than a reachable branch today.
    let (encoded, resized_media_type, resized_dimensions) =
        match maybe_resize_and_downsample_image_buffer(
            &image_buffer,
            original_size,
            detected_format,
        ) {
            Ok(resized) => (
                base64::engine::general_purpose::STANDARD.encode(&resized.buffer),
                resized.media_type,
                resized.dimensions,
            ),
            Err(error) => return Err(error.into()),
        };
    let estimated_tokens = ((encoded.len() as f64) * 0.125).ceil();
    let (base64, media_type, dimensions) = if estimated_tokens > max_tokens {
        // CC next tries token-budget compression from the same original buffer,
        // then a 400x400 quality-20 JPEG, and finally the original buffer.
        match compress_image_buffer_with_token_limit(
            &image_buffer,
            max_tokens,
            Some(&detected_media_type),
        ) {
            Ok(compressed) => (compressed.base64, compressed.media_type, None),
            Err(_) => {
                use image::GenericImageView as _;
                let fallback =
                    (|| -> Result<Vec<u8>, image::ImageError> {
                        let mut reader =
                            image::ImageReader::new(std::io::Cursor::new(&image_buffer))
                                .with_guessed_format()?;
                        reader.limits(image::Limits::no_limits());
                        let decoded = reader.decode()?;
                        let (width, height) = decoded.dimensions();
                        let fallback = if width <= 400 && height <= 400 {
                            decoded
                        } else {
                            decoded.resize(400, 400, image::imageops::FilterType::Lanczos3)
                        };
                        let rgb = fallback.to_rgb8();
                        let (width, height) = rgb.dimensions();
                        let mut output = Vec::new();
                        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 20)
                            .encode(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)?;
                        Ok(output)
                    })();
                match fallback {
                    Ok(buffer) => (
                        base64::engine::general_purpose::STANDARD.encode(buffer),
                        "image/jpeg".to_string(),
                        None,
                    ),
                    Err(_) => (
                        base64::engine::general_purpose::STANDARD.encode(&image_buffer),
                        detected_media_type,
                        None,
                    ),
                }
            }
        }
    } else {
        (encoded, resized_media_type, resized_dimensions)
    };

    Ok(ReadImageOutput {
        base64,
        media_type,
        original_size,
        dimensions,
    })
}

fn is_unc_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

/// Behavioral half of CC `FileReadTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct FileReadTool;

impl FileReadTool {
    /// Maps to: CC `tools/FileReadTool/FileReadTool.ts:496-648`
    /// `FileReadTool.call`.
    ///
    /// L1 (`FileReadTool fallible source-position state carrier`): CC shared
    /// mutable context + throw ≙ independently Arc-shared source turns plus a
    /// lossless fallible Rust result. This is the sole Read call pipeline.
    pub(crate) async fn call(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
        _parent_message: Option<&crate::types::message::AssistantMessage>,
    ) -> Result<FileReadToolResult, FileReadCallFailure> {
        let input = FileReadInput::from_args(args)
            .map_err(|error| FileReadCallFailure::new(anyhow::Error::msg(error)))?;
        let default_limits = get_default_file_reading_limits();
        let limits = FileReadingLimits {
            max_tokens: context
                .file_reading_limits
                .as_ref()
                .and_then(|limits| limits.max_tokens)
                .unwrap_or(default_limits.max_tokens),
            max_size_bytes: context
                .file_reading_limits
                .as_ref()
                .and_then(|limits| limits.max_size_bytes)
                .unwrap_or(default_limits.max_size_bytes),
            include_max_size_in_prompt: default_limits.include_max_size_in_prompt,
            targeted_range_nudge: default_limits.targeted_range_nudge,
        };
        let full_file_path =
            crate::utils::path::expand_path(&input.file_path, context.cwd_override.as_deref())
                .map_err(|error| FileReadCallFailure::new(anyhow::Error::msg(error)))?;
        let sink = FileReadSourcePositionSink { context };

        let dedup_killswitch = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::ReadDedupKillswitch,
        );
        let existing = (!dedup_killswitch)
            .then(|| sink.cache_get(&full_file_path))
            .flatten();
        if let Some(existing) = existing {
            if !existing.is_partial_view && existing.offset.is_some() {
                let range_match = existing
                    .offset
                    .as_ref()
                    .is_some_and(|offset| javascript_strict_equal(offset, &input.offset))
                    && match (&existing.limit, &input.limit) {
                        (None, None) => true,
                        (Some(existing), Some(input)) => javascript_strict_equal(existing, input),
                        _ => false,
                    };
                let unchanged = range_match
                    && crate::utils::file::get_file_modification_time_result(&full_file_path)
                        .ok()
                        .is_some_and(|mtime| existing.timestamp_ms == Some(mtime));
                if unchanged {
                    return Ok(FileReadToolResult {
                        data: ReadOutput::FileUnchanged(ReadFileUnchangedOutput {
                            file_path: input.file_path.clone(),
                        }),
                        new_messages: Vec::new(),
                        memory_file_mtime_ms: None,
                    });
                }
            }
        }

        // Maps to CC `FileReadTool.ts:574-591`: discovery settles first;
        // trigger insertion precedes the fire-and-forget loader invocation;
        // conditional activation then runs synchronously. Simple mode skips
        // the complete block. There is no separate dynamic-discovery gate.
        if !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
        ) {
            let cwd = context.effective_cwd();
            let paths = vec![full_file_path.clone()];
            let worker_cwd = cwd.clone();
            let worker_paths = paths.clone();
            let directories = tokio::task::spawn_blocking(move || {
                crate::skills::load_skills_dir::discover_skill_dirs_for_paths(
                    &worker_paths,
                    &worker_cwd,
                )
            })
            .await
            .map_err(|error| {
                FileReadCallFailure::new(anyhow::anyhow!(
                    "Read skill discovery worker failed: {error}"
                ))
            })?;
            sink.add_dynamic_skill_dirs(&directories);
            if !directories.is_empty() {
                let directories_to_load = directories.clone();
                let _ = std::thread::Builder::new()
                    .name("read-skill-loader".to_string())
                    .spawn(move || {
                        crate::skills::load_skills_dir::add_skill_directories(&directories_to_load);
                    });
            }
            let _ =
                crate::skills::load_skills_dir::activate_conditional_skills_for_paths(&paths, &cwd);
        }

        match self
            .call_inner(&input, &full_file_path, &full_file_path, limits, context)
            .await
        {
            Ok(result) => Ok(result),
            Err(error) if error.is_enoent() => {
                if let Some(alternate) = get_alternate_screenshot_path(&full_file_path) {
                    match self
                        .call_inner(&input, &full_file_path, &alternate, limits, context)
                        .await
                    {
                        Ok(result) => return Ok(result),
                        Err(alternate_error) if !alternate_error.is_enoent() => {
                            return Err(alternate_error);
                        }
                        Err(_) => {}
                    }
                }
                // CC performs the sibling scan before awaiting the cwd-path
                // suggestion even though the latter wins the displayed copy.
                let similar = crate::utils::file::find_similar_file(&full_file_path);
                let cwd = context.effective_cwd();
                let suggestion = crate::utils::file::suggest_path_under_cwd(&full_file_path, &cwd);
                let mut message = format!(
                    "File does not exist. {} {}.",
                    crate::utils::file::FILE_NOT_FOUND_CWD_NOTE,
                    cwd.display()
                );
                if let Some(suggestion) = suggestion {
                    message.push_str(&format!(" Did you mean {}?", suggestion.display()));
                } else if let Some(similar) = similar {
                    message.push_str(&format!(" Did you mean {similar}?"));
                }
                Err(error.context(message))
            }
            Err(error) => Err(error),
        }
    }

    /// Maps to: CC `tools/FileReadTool/FileReadTool.ts:804-1086`
    /// `callInner`, with Rust's required blocking-I/O task boundary.
    async fn call_inner(
        &self,
        input: &FileReadInput,
        full_file_path: &std::path::Path,
        resolved_file_path: &std::path::Path,
        limits: FileReadingLimits,
        context: &crate::tool::ToolUseContext,
    ) -> Result<FileReadToolResult, FileReadCallFailure> {
        let worker_input = input.clone();
        let worker_full_path = full_file_path.to_path_buf();
        let worker_resolved_path = resolved_file_path.to_path_buf();
        let worker_context = context.clone();
        let mut prepared = tokio::task::spawn_blocking(move || {
            let input = &worker_input;
            let full_file_path = worker_full_path.as_path();
            let resolved_file_path = worker_resolved_path.as_path();
            let context = &worker_context;
            let ext = std::path::Path::new(&input.file_path)
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let nested_path = full_file_path.display().to_string();

            if ext == "ipynb" {
                let cells = crate::utils::notebook::read_notebook(resolved_file_path, None)?;
                let cells_json = serde_json::to_string(&cells)
                    .map_err(|error| anyhow::anyhow!("Error serializing notebook: {error}"))?;
                let cells_json_bytes = cells_json.len() as u64;
                if cells_json_bytes as f64 > limits.max_size_bytes {
                    return Err(anyhow::anyhow!(
                        "Notebook content ({}) exceeds maximum allowed size ({}). \
                         Use Bash with jq to read specific portions:\n  \
                         cat \"{}\" | jq '.cells[:20]' # First 20 cells\n  \
                         cat \"{}\" | jq '.cells[100:120]' # Cells 100-120\n  \
                         cat \"{}\" | jq '.cells | length' # Count total cells\n  \
                         cat \"{}\" | jq '.cells[] | select(.cell_type==\"code\") | .source' # All code sources",
                        format_file_size(cells_json_bytes),
                        format_file_size(limits.max_size_bytes),
                        input.file_path,
                        input.file_path,
                        input.file_path,
                        input.file_path,
                    ));
                }
                let output = ReadNotebookOutput {
                    file_path: input.file_path.clone(),
                    cells,
                };
                return Ok(PreparedReadOutput::Notebook {
                    output,
                    cache_entry: crate::utils::query_helpers::ReadFileStateEntry {
                        path: nested_path.clone(),
                        content: Some(cells_json),
                        timestamp_ms: None,
                        offset: Some(input.offset.clone()),
                        limit: input.limit.clone(),
                        is_partial_view: false,
                        source: crate::utils::query_helpers::ReadFileStateSource::Read,
                    },
                    nested_path,
                });
            }
            if IMAGE_EXTENSIONS.contains(ext.as_str()) {
                let output =
                    read_image_with_token_budget(resolved_file_path, limits.max_tokens, None)?;
                let metadata_text = output
                    .dimensions
                    .as_ref()
                    .and_then(|dimensions| create_image_metadata_text(dimensions, None));
                return Ok(PreparedReadOutput::Image {
                    output,
                    metadata_text,
                    nested_path,
                });
            }
            // Maps to: CC `tools/FileReadTool/FileReadTool.ts:890-1009`
            // `callInner` PDF branch. Pages mode enters extraction before any
            // Read-owned stat; full mode counts pages before its local stat.
            if crate::utils::pdf_utils::is_pdf_extension(&ext) {
                use crate::constants::api_limits::{
                    PDF_AT_MENTION_INLINE_THRESHOLD, PDF_EXTRACT_SIZE_THRESHOLD,
                    PDF_MAX_PAGES_PER_READ,
                };
                use crate::utils::pdf::{extract_pdf_pages, get_pdf_page_count, read_pdf};
                use crate::utils::pdf_utils::{is_pdf_supported, parse_pdf_page_range};

                if input.pages.as_ref().is_some_and(javascript_truthy) {
                    let pages = input
                        .pages
                        .as_ref()
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("pages.trim is not a function"))?;
                    let range = parse_pdf_page_range(pages);
                    let extract = futures::executor::block_on(extract_pdf_pages(resolved_file_path, range))
                        .map_err(|error| anyhow::anyhow!(error.message))?;
                    let entries = std::fs::read_dir(&extract.output_dir)?
                        .collect::<std::io::Result<Vec<_>>>()?;
                    let mut names = entries
                        .into_iter()
                        .filter_map(|entry| {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            name.ends_with(".jpg").then_some(name)
                        })
                        .collect::<Vec<_>>();
                    names.sort();
                    let page_paths = names
                        .into_iter()
                        .map(|name| extract.output_dir.join(name))
                        .collect::<Vec<_>>();
                    return Ok(PreparedReadOutput::Parts {
                        output: ReadPdfPartsOutput {
                            file_path: resolved_file_path.display().to_string(),
                            original_size: extract.original_size,
                            count: extract.count,
                            output_dir: extract.output_dir,
                        },
                        page_paths,
                    });
                }
                if let Some(page_count) = get_pdf_page_count(resolved_file_path) {
                    if page_count > PDF_AT_MENTION_INLINE_THRESHOLD {
                        return Err(anyhow::anyhow!(
                            "This PDF has {page_count} pages, which is too many to read at once. Use the pages parameter to read specific page ranges (e.g., pages: \"1-5\"). Maximum {PDF_MAX_PAGES_PER_READ} pages per request."
                        ));
                    }
                }
                let opened_metadata = futures::executor::block_on(
                    crate::utils::fs_operations::get_fs_implementation().stat(resolved_file_path),
                )?;
                if !is_pdf_supported() || opened_metadata.len() > PDF_EXTRACT_SIZE_THRESHOLD {
                    let _ = futures::executor::block_on(extract_pdf_pages(resolved_file_path, None));
                }
                if !is_pdf_supported() {
                    return Err(anyhow::anyhow!(
                        "Reading full PDFs is not supported with this model. Use a newer model (Sonnet 3.5 v2 or later), or use the pages parameter to read specific page ranges (e.g., pages: \"1-5\", maximum {PDF_MAX_PAGES_PER_READ} pages per request). Page extraction requires poppler-utils: install with `brew install poppler` on macOS or `apt-get install poppler-utils` on Debian/Ubuntu."
                    ));
                }
                let pdf =
                    futures::executor::block_on(read_pdf(resolved_file_path)).map_err(|error| anyhow::anyhow!(error.message))?;
                return Ok(PreparedReadOutput::Pdf {
                    output: ReadPdfOutput {
                        file_path: resolved_file_path.display().to_string(),
                        base64: pdf.base64,
                        original_size: pdf.original_size,
                    },
                });
            }

            // `offset === 0 ? 0 : offset - 1`: strict equality is
            // intentionally separate from subtraction's JavaScript ToNumber
            // coercion.
            let line_offset = if matches!(
                &input.offset,
                serde_json::Value::Number(number) if number.as_f64() == Some(0.0)
            ) {
                0.0
            } else {
                javascript_to_number(&input.offset) - 1.0
            };
            let result = crate::utils::read_file_in_range::read_file_in_range(
                resolved_file_path,
                line_offset,
                input.limit.clone(),
                input.limit.is_none().then_some(limits.max_size_bytes),
                Some(&context.abort_controller),
                crate::utils::read_file_in_range::ReadFileRangeOptions::default(),
            )?;
            let memory_mtime_ms =
                crate::utils::memory_file_detection::is_auto_mem_file(resolved_file_path)
                    .then_some(result.mtime_ms);
            let output = ReadTextOutput {
                file_path: input.file_path.clone(),
                content: result.content,
                num_lines: result.line_count,
                start_line: input.offset.clone(),
                total_lines: result.total_lines,
            };
            let read_timestamp_ms = result.mtime_ms.floor() as i64;
            Ok(PreparedReadOutput::Text {
                cache_entry: crate::utils::query_helpers::ReadFileStateEntry {
                    path: nested_path,
                    content: Some(output.content.clone()),
                    timestamp_ms: Some(read_timestamp_ms),
                    offset: Some(input.offset.clone()),
                    limit: input.limit.clone(),
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Read,
                },
                output,
                resolved_file_path: resolved_file_path.to_path_buf(),
                nested_path: full_file_path.display().to_string(),
                memory_mtime_ms,
            })
        })
        .await
        .map_err(|error| FileReadCallFailure::new(anyhow::anyhow!("Read worker failed: {error}")))?
        .map_err(FileReadCallFailure::new)?;

        let token_content = match &prepared {
            PreparedReadOutput::Text { output, .. } => Some(output.content.as_str()),
            PreparedReadOutput::Notebook { cache_entry, .. } => cache_entry.content.as_deref(),
            _ => None,
        };
        if let Some(content) = token_content {
            let extension = std::path::Path::new(&input.file_path)
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            validate_content_tokens(content, &extension, limits.max_tokens).await?;
        }

        // CC notebook performs one throwing final stat after token validation.
        if let PreparedReadOutput::Notebook { cache_entry, .. } = &mut prepared {
            cache_entry.timestamp_ms = Some(
                crate::utils::file::get_file_modification_time_result(resolved_file_path)
                    .map_err(|error| FileReadCallFailure::new(anyhow::Error::new(error)))?,
            );
        }

        // `callInner` remains the policy owner. The L1 sink above is limited
        // to source-position cache/Set mechanics and never constructs output
        // variants or supplemental messages.
        match prepared {
            PreparedReadOutput::Text {
                output,
                resolved_file_path,
                nested_path,
                cache_entry,
                memory_mtime_ms,
            } => crate::tool::FileReadSourceTurn::run(|| {
                context.read_file_state.with_cache_in_source_turn(|cache| {
                    let path = std::path::PathBuf::from(&cache_entry.path);
                    cache.set(&path, cache_entry);
                });
                if let Some(triggers) = &context.nested_memory_attachment_triggers {
                    triggers.with_set_in_source_turn(|set| {
                        set.insert(nested_path);
                    });
                }
                let listeners = FILE_READ_LISTENERS
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let listener_path = resolved_file_path.display().to_string();
                for listener in listeners {
                    listener(&listener_path, &output.content).map_err(FileReadCallFailure::new)?;
                }
                Ok(FileReadToolResult {
                    data: ReadOutput::Text(output),
                    new_messages: Vec::new(),
                    memory_file_mtime_ms: memory_mtime_ms,
                })
            }),
            PreparedReadOutput::Notebook {
                output,
                cache_entry,
                nested_path,
            } => crate::tool::FileReadSourceTurn::run(|| {
                context.read_file_state.with_cache_in_source_turn(|cache| {
                    let path = std::path::PathBuf::from(&cache_entry.path);
                    cache.set(&path, cache_entry);
                });
                if let Some(triggers) = &context.nested_memory_attachment_triggers {
                    triggers.with_set_in_source_turn(|set| {
                        set.insert(nested_path);
                    });
                }
                Ok(FileReadToolResult {
                    data: ReadOutput::Notebook(output),
                    new_messages: Vec::new(),
                    memory_file_mtime_ms: None,
                })
            }),
            PreparedReadOutput::Image {
                output,
                metadata_text,
                nested_path,
            } => crate::tool::FileReadSourceTurn::run(|| {
                if let Some(triggers) = &context.nested_memory_attachment_triggers {
                    triggers.with_set_in_source_turn(|set| {
                        set.insert(nested_path);
                    });
                }
                let new_messages = metadata_text
                    .map(|text| {
                        vec![crate::types::message::Message::User(
                            crate::types::message::UserMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::UserContent::MetaText(text)],
                                is_compact_summary: false,
                                plan_content: None,
                                image_paste_ids: None,
                                is_visible_in_transcript_only: false,
                                mcp_meta: None,
                                source_tool_assistant_uuid: None,
                                permission_mode: None,
                                origin: None,
                                summarize_metadata: None,
                            },
                        )]
                    })
                    .unwrap_or_default();
                Ok(FileReadToolResult {
                    data: ReadOutput::Image(output),
                    new_messages,
                    memory_file_mtime_ms: None,
                })
            }),
            PreparedReadOutput::Pdf { output } => Ok(FileReadToolResult {
                new_messages: vec![crate::types::message::Message::User(
                    crate::types::message::UserMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![crate::types::message::UserContent::MetaDocument {
                            media_type: "application/pdf".to_string(),
                            data: output.base64.clone(),
                        }],
                        is_compact_summary: false,
                        plan_content: None,
                        image_paste_ids: None,
                        is_visible_in_transcript_only: false,
                        mcp_meta: None,
                        source_tool_assistant_uuid: None,
                        permission_mode: None,
                        origin: None,
                        summarize_metadata: None,
                    },
                )],
                data: ReadOutput::Pdf(output),
                memory_file_mtime_ms: None,
            }),
            PreparedReadOutput::Parts { output, page_paths } => {
                // Maps to: CC `tools/FileReadTool/FileReadTool.ts:924-961`.
                // `Promise.all(imageFiles.map(...))`: every page
                // read/resize starts concurrently, rejection does not cancel
                // already-started siblings, and successful result order remains
                // the sorted filename order.
                let page_images =
                    futures::future::try_join_all(page_paths.into_iter().map(|path| async move {
                        tokio::task::spawn_blocking(move || {
                            let bytes = std::fs::read(path)?;
                            let resized = maybe_resize_and_downsample_image_buffer(
                                &bytes,
                                bytes.len(),
                                "jpeg",
                            )?;
                            Ok::<_, anyhow::Error>((
                                resized.media_type,
                                base64::engine::general_purpose::STANDARD.encode(&resized.buffer),
                            ))
                        })
                        .await
                        .map_err(|error| anyhow::anyhow!("PDF page worker failed: {error}"))?
                    }))
                    .await
                    .map_err(FileReadCallFailure::new)?;
                let content = page_images
                    .into_iter()
                    .map(
                        |(media_type, data)| crate::types::message::UserContent::MetaImage {
                            media_type,
                            data,
                        },
                    )
                    .collect::<Vec<_>>();
                let new_messages = if content.is_empty() {
                    Vec::new()
                } else {
                    vec![crate::types::message::Message::User(
                        crate::types::message::UserMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content,
                            is_compact_summary: false,
                            plan_content: None,
                            image_paste_ids: None,
                            is_visible_in_transcript_only: false,
                            mcp_meta: None,
                            source_tool_assistant_uuid: None,
                            permission_mode: None,
                            origin: None,
                            summarize_metadata: None,
                        },
                    )]
                };
                Ok(FileReadToolResult {
                    data: ReadOutput::Parts(output),
                    new_messages,
                    memory_file_mtime_ms: None,
                })
            }
        }
    }
}

impl FileReadTool {
    /// Maps to: CC `FileReadTool.ts:652-717`
    /// `mapToolResultToToolResultBlockParam`.
    pub(crate) fn map_tool_result_to_tool_result_block_param(
        data: &ReadOutput,
        memory_mtime_ms: Option<f64>,
        request_model: Option<&str>,
    ) -> Result<FileReadMappedToolResult, FileReadCallFailure> {
        let mapped = match data {
            ReadOutput::Image(output) => FileReadMappedToolResult {
                content: String::new(),
                content_blocks: vec![crate::types::message::ToolResultContentBlock::image_base64(
                    output.media_type.clone(),
                    output.base64.clone(),
                )],
            },
            ReadOutput::Notebook(output) => {
                let blocks = map_notebook_cells_to_tool_result(&output.cells)
                    .map_err(|error| FileReadCallFailure::new(anyhow::Error::msg(error)))?
                    .iter()
                    .map(crate::types::message::ToolResultContentBlock::from_structured_value)
                    .collect();
                FileReadMappedToolResult {
                    content: String::new(),
                    content_blocks: blocks,
                }
            }
            ReadOutput::Pdf(output) => FileReadMappedToolResult {
                content: format!(
                    "PDF file read: {} ({})",
                    output.file_path,
                    format_file_size(output.original_size)
                ),
                content_blocks: Vec::new(),
            },
            ReadOutput::Parts(output) => FileReadMappedToolResult {
                content: format!(
                    "PDF pages extracted: {} page(s) from {} ({})",
                    output.count,
                    output.file_path,
                    format_file_size(output.original_size)
                ),
                content_blocks: Vec::new(),
            },
            ReadOutput::FileUnchanged(_) => FileReadMappedToolResult {
                content: prompt::FILE_UNCHANGED_STUB.to_string(),
                content_blocks: Vec::new(),
            },
            ReadOutput::Text(output) => {
                let content = if !output.content.is_empty() {
                    let mut rendered = memory_mtime_ms
                        .map(crate::memdir::memory_age::memory_freshness_note)
                        .unwrap_or_default();
                    rendered.push_str(&crate::utils::file::add_line_numbers(
                        &output.content,
                        &output.start_line,
                    ));
                    let resolved_model;
                    let model = if let Some(model) = request_model {
                        model
                    } else {
                        resolved_model = crate::utils::model::model::get_main_loop_model();
                        &resolved_model
                    };
                    if crate::utils::model::model::get_canonical_name(model) != "claude-opus-4-6" {
                        rendered.push_str(CYBER_RISK_MITIGATION_REMINDER);
                    }
                    rendered
                } else if output.total_lines == 0 {
                    "<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>"
                        .to_string()
                } else {
                    format!(
                        "<system-reminder>Warning: the file exists but is shorter than the provided offset ({}). The file has {} lines.</system-reminder>",
                        javascript_to_string(&output.start_line),
                        output.total_lines,
                    )
                };
                FileReadMappedToolResult {
                    content,
                    content_blocks: Vec::new(),
                }
            }
        };
        Ok(mapped)
    }
}

impl crate::tool::ToolCall for FileReadTool {
    fn name(&self) -> &'static str {
        prompt::FILE_READ_TOOL_NAME
    }

    /// Maps to: CC `FileReadTool.ts:347-360` `async prompt()` — computed from
    /// the default file-reading limits; see [`api_prompt`].
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        api_prompt()
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("read files, images, PDFs, notebooks")
    }

    /// Maps to: CC `FileReadTool.ts:367` mounting
    /// `UI.tsx:179-188#userFacingName` (`Reading Plan` / `Read agent output` /
    /// `Read`).
    fn user_facing_name(&self, args: Option<&serde_json::Value>) -> String {
        crate::tools::file_read_tool::ui::user_facing_name(args)
    }

    /// Maps to: CC `FileReadTool.getActivityDescription(...)` (:355-358).
    /// Maps to: CC `FileReadTool.ts:88` mounting `UI.tsx#getToolUseSummary`.
    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        crate::tools::file_read_tool::ui::get_tool_use_summary(Some(args))
    }

    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            crate::tools::file_read_tool::ui::get_tool_use_summary(Some(args))
                .map(|summary| format!("Reading {summary}"))
                .unwrap_or_else(|| "Reading file".to_string()),
        )
    }

    /// Maps to: CC `FileReadTool.ts:227-239` Zod semantic number parsing.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut parsed = args.clone();
        for field in ["offset", "limit"] {
            crate::utils::semantic_number::preprocess_object_field(&mut parsed, field);
        }
        parsed
    }

    /// Maps to CC `FileReadTool.backfillObservableInput(...)` (:388-397).
    /// Hooks, permissions, and observable UI receive the expanded absolute
    /// path after initial validation; `PermissionRequest.call_input` keeps the
    /// model's original path when no hook rewrites it.
    fn backfill_observable_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> serde_json::Value {
        let mut observable = args.clone();
        let Some(file_path) = args.get("file_path").and_then(serde_json::Value::as_str) else {
            return observable;
        };
        let Ok(expanded) =
            crate::utils::path::expand_path(file_path, Some(&context.effective_cwd()))
        else {
            // Execution validates the parsed input before this observable
            // clone is exposed. Do not fabricate a cwd-relative fallback.
            return observable;
        };
        if let Some(object) = observable.as_object_mut() {
            object.insert(
                "file_path".to_string(),
                serde_json::Value::String(expanded.display().to_string()),
            );
        }
        observable
    }

    /// Maps to: CC `FileReadTool.isConcurrencySafe(...)` read-only default.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to CC `FileReadTool.isSearchOrReadCommand(...)` (:367-369).
    fn is_search_or_read_command(
        &self,
        _args: &serde_json::Value,
    ) -> Option<crate::tool::SearchOrReadCommand> {
        Some(crate::tool::SearchOrReadCommand {
            is_search: false,
            is_read: true,
        })
    }

    /// Maps to: CC `FileReadTool.isReadOnly(...)` — always true.
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `FileReadTool.maxResultSizeChars = Infinity`.
    fn max_result_size_chars(&self) -> usize {
        crate::tool::UNBOUNDED_MAX_RESULT_SIZE_CHARS
    }

    /// Maps to CC `FileReadTool.getPath(...)` (:371-374), including the
    /// falsey-path cwd fallback used by permission summaries.
    fn get_path(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            args.get("file_path")
                .and_then(serde_json::Value::as_str)
                .filter(|path| !path.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    crate::bootstrap::state::get_original_cwd()
                        .display()
                        .to_string()
                }),
        )
    }

    /// Maps to CC `FileReadTool.toAutoClassifierInput(...)` (:364-366).
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("file_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// Maps to CC `FileReadTool.preparePermissionMatcher(...)` (:383-385).
    fn prepare_permission_matcher(
        &self,
        args: &serde_json::Value,
    ) -> Option<crate::tool::PermissionPatternMatcher> {
        let file_path = args.get("file_path")?.as_str()?.to_string();
        Some(Box::new(move |pattern| {
            crate::utils::permissions::shell_rule_matching::match_wildcard_pattern(
                pattern, &file_path, false,
            )
        }))
    }

    /// Maps to CC `FileReadTool.validateInput(...)` (:418-495). This executes
    /// on the initially parsed input before observable backfill and PreToolUse.
    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        use crate::constants::api_limits::PDF_MAX_PAGES_PER_READ;
        use crate::types::permissions::PermissionBehavior;
        use crate::utils::pdf_utils::parse_pdf_page_range;
        use crate::utils::permissions::filesystem::{
            FilePermissionType, matching_rule_for_input,
        };

        let Ok(input) = FileReadInput::from_args(args) else {
            return crate::tool::ValidationResult::fatal("Invalid Read tool input");
        };
        if let Some(pages) = input.pages.as_ref().and_then(serde_json::Value::as_str) {
            let Some(parsed) = parse_pdf_page_range(pages) else {
                return crate::tool::ValidationResult::error(
                    format!(
                        "Invalid pages parameter: \"{pages}\". Use formats like \"1-5\", \"3\", or \"10-20\". Pages are 1-indexed."
                    ),
                    7,
                );
            };
            let range_size = parsed
                .last_page
                .map(|last_page| last_page - parsed.first_page + 1.0)
                .unwrap_or(f64::from(PDF_MAX_PAGES_PER_READ.saturating_add(1)));
            if range_size > f64::from(PDF_MAX_PAGES_PER_READ) {
                return crate::tool::ValidationResult::error(
                    format!(
                        "Page range \"{pages}\" exceeds maximum of {PDF_MAX_PAGES_PER_READ} pages per request. Please use a smaller range."
                    ),
                    8,
                );
            }
        }

        let full_file_path = match crate::utils::path::expand_path(
            &input.file_path,
            context.cwd_override.as_deref(),
        ) {
            Ok(path) => path,
            Err(message) => return crate::tool::ValidationResult::fatal(message),
        };
        let full_file_path_str = full_file_path.display().to_string();
        if matching_rule_for_input(
            &full_file_path_str,
            &context.tool_permission_context,
            FilePermissionType::Read,
            PermissionBehavior::Deny,
            &context.effective_cwd(),
        )
        .is_some()
        {
            return crate::tool::ValidationResult::error(
                "File is in a directory that is denied by your permission settings.",
                1,
            );
        }
        if is_unc_path(&full_file_path_str) {
            return crate::tool::ValidationResult::Ok;
        }

        let ext = full_file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let ext_with_dot = format!(".{ext}");
        if crate::constants::files::has_binary_extension(&full_file_path_str)
            && !crate::utils::pdf_utils::is_pdf_extension(&ext_with_dot)
            && !IMAGE_EXTENSIONS.contains(ext.as_str())
        {
            return crate::tool::ValidationResult::error(
                format!(
                    "This tool cannot read binary files. The file appears to be a binary {ext_with_dot} file. Please use appropriate tools for binary file analysis."
                ),
                4,
            );
        }
        if is_blocked_device_path(&full_file_path_str) {
            return crate::tool::ValidationResult::error(
                format!(
                    "Cannot read '{}': this device file would block or produce infinite output.",
                    input.file_path
                ),
                9,
            );
        }
        crate::tool::ValidationResult::Ok
    }

    /// Maps to CC `FileReadTool.checkPermissions(...)` (:386-394) through
    /// `checkReadPermissionForTool(...)`.
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let parsed = self.normalize_input(args);
        let cwd = context.effective_cwd();
        let file_path = parsed
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| cwd.display().to_string());
        crate::utils::permissions::filesystem::check_read_permission_for_tool(
            &file_path,
            &parsed,
            &context.tool_permission_context,
            &cwd,
        )
    }

    /// Generic registry adapter only. The full Read pipeline remains in the
    /// inherent fallible `FileReadTool::call` above.
    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let outcome = match FileReadTool::call(self, args, context, parent_message).await {
                Ok(result) => FileReadRegistryOutcome::Success(result),
                Err(error) => FileReadRegistryOutcome::Failure(error),
            };
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::FileReadCall(outcome),
                new_messages: Vec::new(),
            }
        })
    }

    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::ToolResultStatus;
        match data {
            crate::tool::ToolOutput::FileReadCall(FileReadRegistryOutcome::Success(result)) => {
                match FileReadTool::map_tool_result_to_tool_result_block_param(
                    &result.data,
                    result.memory_file_mtime_ms(),
                    None,
                ) {
                    Ok(mapped) => (mapped.content, ToolResultStatus::Success),
                    Err(error) => (error.message(), ToolResultStatus::Error),
                }
            }
            crate::tool::ToolOutput::FileReadCall(FileReadRegistryOutcome::Failure(error)) => {
                (error.message(), ToolResultStatus::Error)
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (String::new(), ToolResultStatus::Error),
        }
    }

    /// Maps to: CC recording FileReadTool's `Output` as the message's
    /// `toolUseResult`. A failure records the `Error: …` string, matching the
    /// string `toolUseResult` CC keeps for errored reads.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::FileReadCall(FileReadRegistryOutcome::Success(result)) => {
                Some(result.raw_output())
            }
            crate::tool::ToolOutput::FileReadCall(FileReadRegistryOutcome::Failure(error)) => Some(
                serde_json::Value::String(format!("Error: {}", error.message())),
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::sync::atomic::Ordering;

    async fn invoke_text_read_listener_path(
        path: &std::path::Path,
        content: &str,
    ) -> Result<(), FileReadCallFailure> {
        std::fs::write(path, content).map_err(|error| FileReadCallFailure::new(error.into()))?;
        FileReadTool
            .call(
                &serde_json::json!({"file_path": path.display().to_string()}),
                &crate::tool::ToolUseContext::default(),
                None,
            )
            .await
            .map(|_| ())
    }

    #[tokio::test]
    async fn read_listener_snapshot_survives_self_unsubscribe() {
        let first_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_unsubscribe = Arc::new(Mutex::new(None::<Box<dyn FnOnce() + Send + 'static>>));
        let first_calls_for_listener = Arc::clone(&first_calls);
        let unsubscribe_for_listener = Arc::clone(&first_unsubscribe);
        let unsubscribe_first = register_file_read_listener(Arc::new(move |path, _| {
            if path != "/tmp/listener.txt" {
                return Ok(());
            }
            first_calls_for_listener.fetch_add(1, Ordering::SeqCst);
            if let Some(unsubscribe) = unsubscribe_for_listener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                unsubscribe();
            }
            Ok(())
        }));
        *first_unsubscribe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(unsubscribe_first));
        let second_calls_for_listener = Arc::clone(&second_calls);
        let unsubscribe_second = register_file_read_listener(Arc::new(move |path, _| {
            if path != "/tmp/listener.txt" {
                return Ok(());
            }
            second_calls_for_listener.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));

        let path = std::path::Path::new("/tmp/listener.txt");
        invoke_text_read_listener_path(path, "first").await.unwrap();
        invoke_text_read_listener_path(path, "second")
            .await
            .unwrap();
        let _ = std::fs::remove_file(path);

        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 2);
        unsubscribe_second();
    }

    #[tokio::test]
    async fn duplicate_listener_unsubscribes_remove_first_live_identity_like_official() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_for_shared = Arc::clone(&calls);
        let shared: FileReadListener = Arc::new(move |path, _| {
            if path == "/tmp/listener-identity.txt" {
                calls_for_shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("shared".to_string());
            }
            Ok(())
        });
        let first_unsubscribe = register_file_read_listener(Arc::clone(&shared));
        let calls_for_middle = Arc::clone(&calls);
        let middle_unsubscribe = register_file_read_listener(Arc::new(move |path, _| {
            if path == "/tmp/listener-identity.txt" {
                calls_for_middle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("middle".to_string());
            }
            Ok(())
        }));
        let last_unsubscribe = register_file_read_listener(shared);

        // Both closures search by callback identity, so reverse invocation
        // removes the first shared registration and then the remaining one.
        last_unsubscribe();
        first_unsubscribe();
        last_unsubscribe();
        let path = std::path::Path::new("/tmp/listener-identity.txt");
        invoke_text_read_listener_path(path, "content")
            .await
            .unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["middle"]
        );
        middle_unsubscribe();
    }

    #[tokio::test]
    async fn listener_registered_during_dispatch_joins_only_the_next_snapshot() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let late_unsubscribe = Arc::new(Mutex::new(None::<Box<dyn Fn() + Send + Sync>>));
        let registered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let calls_for_first = Arc::clone(&calls);
        let calls_for_late = Arc::clone(&calls);
        let late_unsubscribe_for_first = Arc::clone(&late_unsubscribe);
        let registered_for_first = Arc::clone(&registered);
        let first_unsubscribe = register_file_read_listener(Arc::new(move |path, _| {
            if path != "/tmp/listener-registration.txt" {
                return Ok(());
            }
            calls_for_first
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push("first".to_string());
            if !registered_for_first.swap(true, Ordering::SeqCst) {
                let calls_for_late = Arc::clone(&calls_for_late);
                let unsubscribe = register_file_read_listener(Arc::new(move |path, _| {
                    if path == "/tmp/listener-registration.txt" {
                        calls_for_late
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push("late".to_string());
                    }
                    Ok(())
                }));
                *late_unsubscribe_for_first
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(Box::new(unsubscribe));
            }
            Ok(())
        }));
        let calls_for_second = Arc::clone(&calls);
        let second_unsubscribe = register_file_read_listener(Arc::new(move |path, _| {
            if path == "/tmp/listener-registration.txt" {
                calls_for_second
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("second".to_string());
            }
            Ok(())
        }));

        let path = std::path::Path::new("/tmp/listener-registration.txt");
        invoke_text_read_listener_path(path, "first").await.unwrap();
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["first", "second"]
        );
        invoke_text_read_listener_path(path, "second")
            .await
            .unwrap();
        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["first", "second", "first", "second", "late"]
        );

        first_unsubscribe();
        second_unsubscribe();
        let late_unsubscribe = late_unsubscribe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(unsubscribe) = late_unsubscribe {
            unsubscribe();
        }
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn read_listener_stops_on_first_error_and_unsubscribe_is_repeatable() {
        let later_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let unsubscribe_error = register_file_read_listener(Arc::new(|path, _| {
            if path == "/tmp/listener-error.txt" {
                Err(anyhow::anyhow!("listener failed"))
            } else {
                Ok(())
            }
        }));
        let later_calls_for_listener = Arc::clone(&later_calls);
        let unsubscribe_later = register_file_read_listener(Arc::new(move |path, _| {
            if path == "/tmp/listener-error.txt" {
                later_calls_for_listener.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }));

        let path = std::path::Path::new("/tmp/listener-error.txt");
        let error = invoke_text_read_listener_path(path, "content")
            .await
            .unwrap_err();
        assert_eq!(error.message(), "listener failed");
        let _ = std::fs::remove_file(path);
        assert_eq!(later_calls.load(Ordering::SeqCst), 0);
        unsubscribe_error();
        unsubscribe_error();
        unsubscribe_later();
        unsubscribe_later();
    }

    #[test]
    fn read_number_stringification_delegates_full_ecmascript_domain_to_ryu_js() {
        for (value, expected) in [
            (-0.0, "0"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (1e-7, "1e-7"),
            (1e21, "1e+21"),
        ] {
            let rendered = MaxFileReadTokenExceededError {
                token_count: 42,
                max_tokens: value,
            }
            .to_string();
            let actual = rendered
                .strip_prefix("File content (42 tokens) exceeds maximum allowed tokens (")
                .and_then(|value| value.split_once("). Use offset and limit parameters"))
                .map(|(value, _)| value)
                .expect("stable MaxFileReadTokenExceededError copy");
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn file_read_tool_schema_matches_official_input_shape() {
        let schema = file_read_tool_schema();
        assert_eq!(schema.name, "Read");
        assert!(schema.strict.unwrap());
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["file_path"])
        );
        for key in ["file_path", "offset", "limit", "pages"] {
            assert!(
                schema
                    .input_schema
                    .pointer(&format!("/properties/{key}"))
                    .is_some(),
                "missing {key}"
            );
        }
        assert_eq!(
            schema.input_schema["properties"]["offset"]["type"],
            "integer"
        );
        assert_eq!(schema.input_schema["properties"]["offset"]["minimum"], 0);
        assert_eq!(
            schema.input_schema["properties"]["offset"]["maximum"],
            9_007_199_254_740_991_u64
        );
        assert_eq!(
            schema.input_schema["properties"]["limit"]["type"],
            "integer"
        );
        assert_eq!(
            schema.input_schema["properties"]["limit"]["exclusiveMinimum"],
            0
        );
        assert_eq!(
            schema.input_schema["properties"]["limit"]["maximum"],
            9_007_199_254_740_991_u64
        );
    }

    #[test]
    fn file_read_semantic_number_preprocessing_matches_official_schema() {
        let parsed = crate::tool::ToolCall::normalize_input(
            &FileReadTool,
            &serde_json::json!({
                "file_path": "/tmp/a",
                "offset": "0",
                "limit": "4.0"
            }),
        );
        assert_eq!(parsed["offset"], serde_json::json!(0));
        assert_eq!(parsed["limit"], serde_json::json!(4));
    }

    #[tokio::test]
    async fn text_read_applies_offset_limit_window() {
        let dir = std::env::temp_dir().join(format!("cometix-read-text-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("sample.txt");
        std::fs::write(&path, "a\nb\nc\nd\n").expect("write");

        let input = FileReadInput {
            file_path: path.to_str().unwrap().to_string(),
            offset: serde_json::json!(2),
            limit: Some(serde_json::json!(2)),
            pages: None,
        };
        let context = crate::tool::ToolUseContext::default();
        let result = FileReadTool
            .call_inner(
                &input,
                &path,
                &path,
                get_default_file_reading_limits(),
                &context,
            )
            .await
            .expect("read");
        let ReadOutput::Text(output) = result.data else {
            panic!("expected text output");
        };
        assert_eq!(output.content, "b\nc");
        assert_eq!(output.num_lines, 2);
        assert_eq!(output.start_line, serde_json::json!(2));
        // `readFileInRangeFast` counts the final empty fragment after a
        // trailing newline.
        assert_eq!(output.total_lines, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn text_read_rejects_oversized_file_without_limit() {
        let dir = std::env::temp_dir().join(format!("cometix-read-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("big.txt");
        let content = "x".repeat(1024);
        std::fs::write(&path, &content).expect("write");

        let mut context = crate::tool::ToolUseContext::default();
        context.file_reading_limits = Some(crate::tool::FileReadingLimitsOverride {
            max_tokens: None,
            max_size_bytes: Some(512.0),
        });
        let err = FileReadTool
            .call(
                &serde_json::json!({"file_path": path.display().to_string()}),
                &context,
                None,
            )
            .await
            .expect_err("should reject");
        assert!(err.is_file_too_large(), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_device_paths_match_official_set() {
        assert!(is_blocked_device_path("/dev/zero"));
        assert!(is_blocked_device_path("/proc/self/fd/0"));
        assert!(!is_blocked_device_path("/dev/null"));
    }

    #[test]
    fn mitigation_exemption_uses_exact_canonical_model_identity() {
        let output = ReadOutput::Text(ReadTextOutput {
            file_path: "/tmp/example".to_string(),
            content: "content".to_string(),
            num_lines: 1,
            start_line: serde_json::json!(1),
            total_lines: 1,
        });
        let exempt = FileReadTool::map_tool_result_to_tool_result_block_param(
            &output,
            None,
            Some("us.anthropic.claude-opus-4-6-v1:0"),
        )
        .expect("exempt mapping")
        .content;
        assert!(!exempt.contains(CYBER_RISK_MITIGATION_REMINDER));

        let noncanonical = FileReadTool::map_tool_result_to_tool_result_block_param(
            &output,
            None,
            Some("custom-opus-4-6-experiment"),
        )
        .expect("noncanonical mapping")
        .content;
        assert!(noncanonical.contains(CYBER_RISK_MITIGATION_REMINDER));
    }

    #[tokio::test]
    async fn notebook_read_returns_cells() {
        let dir = std::env::temp_dir().join(format!("cometix-read-nb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("demo.ipynb");
        std::fs::write(
            &path,
            r#"{"metadata":{"language_info":{"name":"python"}},"cells":[{"cell_type":"code","source":["1"],"outputs":[]}]}"#,
        )
        .expect("write");
        let input = FileReadInput {
            file_path: path.to_str().unwrap().to_string(),
            offset: serde_json::json!(1),
            limit: None,
            pages: None,
        };
        let context = crate::tool::ToolUseContext::default();
        let mut limits = get_default_file_reading_limits();
        limits.max_size_bytes = 256.0 * 1024.0;
        let result = FileReadTool
            .call_inner(&input, &path, &path, limits, &context)
            .await
            .expect("nb");
        let ReadOutput::Notebook(output) = result.data else {
            panic!("expected notebook output");
        };
        assert_eq!(output.cells.len(), 1);
        assert_eq!(
            output.cells[0].source,
            Some(serde_json::Value::String("1".to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_input_order_matches_official_pages_path_and_binary_checks() {
        let context = crate::tool::ToolUseContext::default();
        let validate = |input: serde_json::Value| {
            crate::tool::ToolCall::validate_input(&FileReadTool, &input, &context)
        };
        assert!(matches!(
            validate(serde_json::json!({"file_path": "/tmp/a.pdf", "pages": "0"})),
            crate::tool::ValidationResult::Error { error_code: 7, .. }
        ));
        assert!(matches!(
            validate(serde_json::json!({"file_path": "/tmp/a.pdf", "pages": "1-50"})),
            crate::tool::ValidationResult::Error { error_code: 8, .. }
        ));
        assert!(matches!(
            validate(serde_json::json!({"file_path": "/tmp/tool.exe"})),
            crate::tool::ValidationResult::Error { error_code: 4, .. }
        ));
        assert!(validate(serde_json::json!({"file_path": "/tmp/shot.png"})).is_ok());
        assert!(matches!(
            validate(serde_json::json!({"file_path": "bad\0path", "pages": "0"})),
            crate::tool::ValidationResult::Error { error_code: 7, .. }
        ));
        assert_eq!(
            validate(serde_json::json!({"file_path": "bad\0path"})),
            crate::tool::ValidationResult::fatal("Path contains null bytes")
        );
    }

    #[tokio::test]
    async fn max_token_failure_has_typed_identity_and_commits_no_success_effects() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-read-max-token-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("large.txt");
        std::fs::write(&path, "token ".repeat(1_000)).unwrap();
        let mut context = crate::tool::ToolUseContext::default();
        context.file_reading_limits = Some(crate::tool::FileReadingLimitsOverride {
            max_tokens: Some(1.0),
            max_size_bytes: None,
        });

        let error = FileReadTool
            .call(
                &serde_json::json!({"file_path": path.display().to_string()}),
                &context,
                None,
            )
            .await
            .unwrap_err();
        assert!(error.is_max_token_exceeded());
        assert!(
            error
                .downcast_ref::<MaxFileReadTokenExceededError>()
                .is_some()
        );
        assert!(context.read_file_state.is_empty());
        assert!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn preaborted_read_retains_reached_dynamic_skill_insertion_only() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "cometix-read-preabort-effects-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skill_dir = root.join("packages/pkg/.claude/skills");
        let source = root.join("packages/pkg/src/value.txt");
        std::fs::create_dir_all(skill_dir.join("sample")).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(skill_dir.join("sample/SKILL.md"), "# Sample").unwrap();
        std::fs::write(&source, "content\n").unwrap();
        let context = crate::tool::ToolUseContext::default().with_cwd_override(Some(root.clone()));
        context.abort_controller.abort();

        let error = FileReadTool
            .call(
                &serde_json::json!({"file_path": source.display().to_string()}),
                &context,
                None,
            )
            .await
            .unwrap_err();
        assert!(
            crate::utils::errors::is_abort_error(&error.source)
                || error.message().contains("aborted")
        );
        assert!(
            context
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .contains(&skill_dir.display().to_string())
        );
        assert!(context.read_file_state.is_empty());
        assert!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn listener_failure_preserves_error_identity_after_prior_success_effects() {
        #[derive(Debug)]
        struct ListenerFailure;
        impl std::fmt::Display for ListenerFailure {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("listener identity")
            }
        }
        impl std::error::Error for ListenerFailure {}

        let dir = std::env::temp_dir().join(format!(
            "cometix-read-listener-failure-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.txt");
        std::fs::write(&path, "content\n").unwrap();
        let target = path.display().to_string();
        let target_for_listener = target.clone();
        let unsubscribe = register_file_read_listener(Arc::new(move |path, _| {
            if path == target_for_listener {
                Err(anyhow::Error::new(ListenerFailure))
            } else {
                Ok(())
            }
        }));
        let context = crate::tool::ToolUseContext::default();
        let error = FileReadTool
            .call(&serde_json::json!({"file_path": target}), &context, None)
            .await
            .unwrap_err();
        unsubscribe();

        assert!(error.downcast_ref::<ListenerFailure>().is_some());
        assert!(context.read_file_state.has(&path));
        assert!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .contains(&path.display().to_string())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn listener_enoent_identity_survives_after_text_effects() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-read-listener-enoent-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.txt");
        std::fs::write(&path, "content\n").unwrap();
        let target = path.display().to_string();
        let target_for_listener = target.clone();
        let unsubscribe = register_file_read_listener(Arc::new(move |path, _| {
            if path == target_for_listener {
                Err(anyhow::Error::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "listener ENOENT",
                )))
            } else {
                Ok(())
            }
        }));
        let context = crate::tool::ToolUseContext::default();

        let error = FileReadTool
            .call(&serde_json::json!({"file_path": target}), &context, None)
            .await
            .unwrap_err();
        unsubscribe();

        assert!(error.is_enoent());
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound)
        );
        assert!(context.read_file_state.has(&path));
        assert!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .contains(&path.display().to_string())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn raw_output_schema_contains_no_rust_transport_fields() {
        let value = ReadOutput::Text(ReadTextOutput {
            file_path: "/tmp/source.txt".to_string(),
            content: "content".to_string(),
            num_lines: 1,
            start_line: serde_json::json!(1),
            total_lines: 1,
        })
        .to_output_schema_value();
        assert_eq!(
            value.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["type", "file"]
        );
        let file = value["file"].as_object().unwrap();
        assert_eq!(
            file.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["filePath", "content", "numLines", "startLine", "totalLines"]
        );
        let image = ReadOutput::Image(ReadImageOutput {
            base64: "aW1n".to_string(),
            media_type: "image/png".to_string(),
            original_size: 3,
            dimensions: None,
        })
        .to_output_schema_value();
        assert!(image["file"].get("dimensions").is_none());
    }

    #[tokio::test]
    async fn raw_read_hook_values_matches_official_javascript_runtime() {
        struct Case {
            name: &'static str,
            offset: serde_json::Value,
            limit: Option<serde_json::Value>,
            expected_content: &'static str,
            expected_numbered: Option<&'static str>,
            output_schema_valid: bool,
        }

        let cases = vec![
            Case {
                name: "integer",
                offset: serde_json::json!(2),
                limit: Some(serde_json::json!(2)),
                expected_content: "b\nc",
                expected_numbered: Some("2\tb\n3\tc"),
                output_schema_valid: true,
            },
            Case {
                name: "string",
                offset: serde_json::json!("2"),
                limit: Some(serde_json::json!("1")),
                expected_content: "b\nc\nd\ne",
                expected_numbered: Some("02\tb\n12\tc\n22\td\n32\te"),
                output_schema_valid: false,
            },
            Case {
                name: "fraction",
                offset: serde_json::json!(1.5),
                limit: Some(serde_json::json!(2.5)),
                expected_content: "b\nc",
                expected_numbered: Some("1.5\tb\n2.5\tc"),
                output_schema_valid: true,
            },
            Case {
                name: "negative",
                offset: serde_json::json!(-1),
                limit: Some(serde_json::json!(1)),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: true,
            },
            Case {
                name: "zero",
                offset: serde_json::json!(0),
                limit: None,
                expected_content: "a\nb\nc\nd\ne",
                expected_numbered: Some("0\ta\n1\tb\n2\tc\n3\td\n4\te"),
                output_schema_valid: true,
            },
            Case {
                name: "null",
                offset: serde_json::Value::Null,
                limit: Some(serde_json::json!(1)),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: false,
            },
            Case {
                name: "null-with-undefined-limit",
                offset: serde_json::Value::Null,
                limit: None,
                expected_content: "a\nb\nc\nd\ne",
                expected_numbered: Some("0\ta\n1\tb\n2\tc\n3\td\n4\te"),
                output_schema_valid: false,
            },
            Case {
                name: "true",
                offset: serde_json::json!(true),
                limit: Some(serde_json::json!(1)),
                expected_content: "a",
                expected_numbered: Some("1\ta"),
                output_schema_valid: false,
            },
            Case {
                name: "false",
                offset: serde_json::json!(false),
                limit: Some(serde_json::json!(1)),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: false,
            },
            Case {
                name: "empty-array",
                offset: serde_json::json!([]),
                limit: Some(serde_json::json!(1)),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: false,
            },
            Case {
                name: "single-array",
                offset: serde_json::json!([2]),
                limit: Some(serde_json::json!(1)),
                expected_content: "b",
                expected_numbered: Some("02\tb"),
                output_schema_valid: false,
            },
            Case {
                name: "multi-array",
                offset: serde_json::json!([1, 2]),
                limit: Some(serde_json::json!(1)),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: false,
            },
            Case {
                name: "nonnumeric",
                offset: serde_json::json!("abc"),
                limit: Some(serde_json::json!(1)),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: false,
            },
            Case {
                name: "non-javascript-negative-infinity-spelling",
                offset: serde_json::json!("-inf"),
                limit: None,
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: false,
            },
            Case {
                name: "negative-infinity",
                offset: serde_json::json!("-Infinity"),
                limit: None,
                expected_content: "a\nb\nc\nd\ne",
                expected_numbered: Some(
                    "0-Infinity\ta\n1-Infinity\tb\n2-Infinity\tc\n3-Infinity\td\n4-Infinity\te",
                ),
                output_schema_valid: false,
            },
            Case {
                name: "ecmascript-bom-whitespace",
                offset: serde_json::json!("\u{feff}"),
                limit: None,
                expected_content: "a\nb\nc\nd\ne",
                expected_numbered: Some(
                    "0\u{feff}\ta\n1\u{feff}\tb\n2\u{feff}\tc\n3\u{feff}\td\n4\u{feff}\te",
                ),
                output_schema_valid: false,
            },
            Case {
                name: "unicode-nel-is-not-ecmascript-whitespace",
                offset: serde_json::json!("\u{0085}"),
                limit: None,
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: false,
            },
            Case {
                name: "null-limit",
                offset: serde_json::json!(1),
                limit: Some(serde_json::Value::Null),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: true,
            },
            Case {
                name: "max-safe",
                offset: serde_json::json!(9_007_199_254_740_991_u64),
                limit: Some(serde_json::json!(1)),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: true,
            },
            Case {
                name: "rounded-above-safe",
                offset: serde_json::json!(9_007_199_254_740_993_u64),
                limit: Some(serde_json::json!(1)),
                expected_content: "",
                expected_numbered: None,
                output_schema_valid: true,
            },
        ];

        let root = std::env::temp_dir().join(format!(
            "cometix-read-raw-js-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("sample.txt");
        std::fs::write(&path, "a\nb\nc\nd\ne").unwrap();

        for case in cases {
            let mut args = serde_json::json!({
                "file_path": path.display().to_string(),
                "offset": case.offset.clone(),
            });
            if let Some(limit) = case.limit.clone() {
                args.as_object_mut()
                    .unwrap()
                    .insert("limit".to_string(), limit);
            }
            let context = crate::tool::ToolUseContext::default();
            let result = FileReadTool
                .call(&args, &context, None)
                .await
                .unwrap_or_else(|error| panic!("{}: {}", case.name, error.message()));
            let ReadOutput::Text(text) = &result.data else {
                panic!("{}: expected text", case.name)
            };
            assert_eq!(text.content, case.expected_content, "{} content", case.name);

            let raw = result.raw_output();
            let raw_start = &raw["file"]["startLine"];
            let expected_start = javascript_runtime_value(&case.offset);
            assert_eq!(raw_start, &expected_start, "{} raw offset", case.name);
            if case.name == "rounded-above-safe" {
                assert_eq!(raw_start, &serde_json::json!(9_007_199_254_740_992_u64));
            }
            let state = context
                .read_file_state
                .get(&path)
                .unwrap_or_else(|| panic!("{} cache state", case.name));
            assert_eq!(
                state.offset.as_ref(),
                Some(raw_start),
                "{} state offset",
                case.name
            );
            assert_eq!(
                state.limit,
                case.limit.as_ref().map(javascript_runtime_value),
                "{} state limit",
                case.name
            );

            let mapped = FileReadTool::map_tool_result_to_tool_result_block_param(
                &result.data,
                result.memory_file_mtime_ms(),
                None,
            )
            .unwrap_or_else(|error| panic!("{} mapper: {}", case.name, error.message()));
            if let Some(numbered) = case.expected_numbered {
                let mapped_content = mapped
                    .content
                    .strip_suffix(CYBER_RISK_MITIGATION_REMINDER)
                    .unwrap_or(&mapped.content);
                assert_eq!(mapped_content, numbered, "{} model prefixes", case.name);
            } else {
                assert_eq!(
                    mapped.content,
                    format!(
                        "<system-reminder>Warning: the file exists but is shorter than the provided offset ({}). The file has 5 lines.</system-reminder>",
                        javascript_to_string(raw_start)
                    ),
                    "{} empty-range warning",
                    case.name
                );
            }

            assert_eq!(
                parse_output(&raw).is_some(),
                case.output_schema_valid,
                "{} strict output parser",
                case.name
            );
            // No display shape on either path — visibility is a
            // render-time decision (`transcript_tool_result_should_emit_ui`
            // parses the raw with the tool's own schema).
        }

        // Separate JSON arrays have distinct JS object identities, so a
        // value-equal second hook payload cannot hit strict-equality dedup.
        let context = crate::tool::ToolUseContext::default();
        for _ in 0..2 {
            let result = FileReadTool
                .call(
                    &serde_json::json!({
                        "file_path": path.display().to_string(),
                        "offset": [2],
                        "limit": 1,
                    }),
                    &context,
                    None,
                )
                .await
                .unwrap();
            assert!(matches!(result.data, ReadOutput::Text(_)));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn raw_read_path_error_contract_and_pages_values_match_official_selection() {
        for input in [
            serde_json::json!({}),
            serde_json::json!({"file_path": null}),
            serde_json::json!({"file_path": false}),
            serde_json::json!({"file_path": 5}),
            serde_json::json!({"file_path": []}),
            serde_json::json!({"file_path": {}}),
        ] {
            assert_eq!(
                FileReadInput::from_args(&input),
                Err("Invalid Read tool input".to_string())
            );
        }
        assert_eq!(
            FileReadInput::from_args(&serde_json::json!({"file_path": "a"}))
                .unwrap()
                .offset,
            serde_json::json!(1)
        );
        assert!(
            [
                serde_json::Value::Null,
                serde_json::json!(false),
                serde_json::json!(0),
                serde_json::json!("")
            ]
            .iter()
            .all(|pages| !javascript_truthy(pages))
        );
        assert!(
            [
                serde_json::json!([]),
                serde_json::json!({}),
                serde_json::json!(true),
                serde_json::json!(1),
                serde_json::json!(" ")
            ]
            .iter()
            .all(javascript_truthy)
        );
    }

    /// Minimal one-page PDF that `pdfinfo` / `pdftoppm` accept (same bytes as
    /// the poppler smoke check used while porting `utils/pdf.ts`).
    fn write_minimal_pdf(path: &std::path::Path) {
        let content = b"%PDF-1.1\n\
1 0 obj<< /Type /Catalog /Pages 2 0 R >>endobj\n\
2 0 obj<< /Type /Pages /Kids [3 0 R] /Count 1 >>endobj\n\
3 0 obj<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>endobj\n\
xref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n\
trailer<< /Size 4 /Root 1 0 R >>\nstartxref\n190\n%%EOF\n";
        std::fs::write(path, content).expect("write pdf");
    }

    #[cfg(unix)]
    struct PdfFifoRestore {
        path: Option<EnvVarGuard>,
        read_task: Option<tokio::task::AbortHandle>,
        prefix_marker: Option<std::path::PathBuf>,
        producer_cancel: Option<std::path::PathBuf>,
        fifos: Vec<std::path::PathBuf>,
        workers: Vec<std::thread::JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl Drop for PdfFifoRestore {
        fn drop(&mut self) {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;

            let cleanup_needed = self.read_task.is_some() || !self.workers.is_empty();
            if let Some(cancel) = &self.producer_cancel {
                let _ = std::fs::write(cancel, b"cancel");
            }
            if let Some(task) = self.read_task.take() {
                task.abort();
            }

            let marker_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while cleanup_needed
                && self.fifos.is_empty()
                && std::time::Instant::now() < marker_deadline
            {
                if let Some(prefix) = self
                    .prefix_marker
                    .as_ref()
                    .and_then(|marker| std::fs::read_to_string(marker).ok())
                    .filter(|prefix| !prefix.is_empty())
                {
                    self.fifos = ["-01.jpg", "-02.jpg"]
                        .map(|suffix| std::path::PathBuf::from(format!("{prefix}{suffix}")))
                        .into();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }

            // O_RDWR never waits for a peer. Its read side releases fixture
            // writers blocked in open(), while writing a byte releases product
            // readers left behind by an aborted async task.
            let mut rescue = self.fifos.iter().map(|_| None).collect::<Vec<_>>();
            let rescue_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while cleanup_needed
                && rescue.iter().any(Option::is_none)
                && std::time::Instant::now() < rescue_deadline
            {
                for (slot, fifo) in rescue.iter_mut().zip(&self.fifos) {
                    if slot.is_none() {
                        *slot = std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .custom_flags(libc::O_NONBLOCK)
                            .open(fifo)
                            .ok();
                    }
                }
                if rescue.iter().any(Option::is_none) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            for fifo in rescue.iter_mut().flatten() {
                let _ = fifo.write_all(&[0]);
            }
            // A writer that has not been scheduled yet must not enter a
            // blocking open after the rescue handles close. Unlinking leaves
            // already-open FIFO endpoints usable while making every later open
            // fail immediately.
            for fifo in &self.fifos {
                let _ = std::fs::remove_file(fifo);
            }
            let worker_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while self.workers.iter().any(|worker| !worker.is_finished())
                && std::time::Instant::now() < worker_deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            for worker in self.workers.drain(..) {
                if worker.is_finished() {
                    let _ = worker.join();
                }
            }
            drop(rescue);

            // Availability is PATH-derived, so restore PATH before clearing it.
            drop(self.path.take());
            crate::utils::pdf::reset_pdftoppm_cache();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pdf_fifo_unwind_cleanup_is_bounded() {
        use std::io::Write as _;
        use std::os::unix::ffi::OsStrExt as _;

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "cometix-read-pdf-cleanup-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("cleanup dir");
        let writer_fifo = dir.join("blocked-writer.jpg");
        let reader_fifo = dir.join("blocked-reader.jpg");
        let late_writer_fifo = dir.join("late-writer.jpg");
        for fifo in [&writer_fifo, &reader_fifo, &late_writer_fifo] {
            let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        }

        let (writer_started_tx, writer_started_rx) = std::sync::mpsc::channel();
        let worker_path = writer_fifo.clone();
        let worker = std::thread::spawn(move || {
            let _ = writer_started_tx.send(());
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .open(worker_path)
                .and_then(|mut fifo| fifo.write_all(b"page"));
        });
        writer_started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("writer started");

        let (reader_started_tx, reader_started_rx) = std::sync::mpsc::channel();
        let (reader_done_tx, reader_done_rx) = std::sync::mpsc::channel();
        let reader_path = reader_fifo.clone();
        let read_task = tokio::task::spawn_blocking(move || {
            let _ = reader_started_tx.send(());
            let _ = reader_done_tx.send(std::fs::read(reader_path));
        });
        reader_started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("reader started");

        let (late_done_tx, late_done_rx) = std::sync::mpsc::channel();
        let late_worker = std::thread::spawn({
            let late_writer_fifo = late_writer_fifo.clone();
            move || {
                std::thread::sleep(std::time::Duration::from_millis(750));
                let result = std::fs::OpenOptions::new()
                    .write(true)
                    .open(late_writer_fifo);
                let _ = late_done_tx.send(result);
            }
        });

        let restore = PdfFifoRestore {
            path: None,
            read_task: Some(read_task.abort_handle()),
            prefix_marker: None,
            producer_cancel: None,
            fifos: vec![writer_fifo, reader_fifo, late_writer_fifo],
            workers: vec![worker, late_worker],
        };
        drop(read_task);
        let started = std::time::Instant::now();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _restore = restore;
            panic!("exercise FIFO unwind cleanup");
        }));
        assert!(unwind.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(
            reader_done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("reader cleanup")
                .expect("reader result"),
            [0]
        );
        assert_eq!(
            late_done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("late writer cleanup")
                .expect_err("unlinked FIFO must reject a late writer")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn pdf_full_primary_and_document_supplement_matches_official_placement() {
        let dir = std::env::temp_dir().join(format!("cometix-read-pdf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("doc.pdf");
        write_minimal_pdf(&path);
        let path_str = path.display().to_string();

        let result = FileReadTool
            .call(
                &serde_json::json!({ "file_path": path_str }),
                &crate::tool::ToolUseContext::default(),
                None,
            )
            .await
            .expect("pdf read");
        let ReadOutput::Pdf(pdf) = &result.data else {
            panic!("expected Pdf, got {:?}", result.data);
        };
        assert_eq!(pdf.file_path, path_str);
        assert!(pdf.original_size > 0);
        assert!(!pdf.base64.is_empty());

        let mapped = FileReadTool::map_tool_result_to_tool_result_block_param(
            &result.data,
            result.memory_file_mtime_ms(),
            None,
        )
        .expect("PDF metadata mapper");
        assert_eq!(
            mapped.content,
            format!(
                "PDF file read: {} ({})",
                path_str,
                format_file_size(pdf.original_size)
            )
        );
        assert!(mapped.content_blocks.is_empty());
        assert_eq!(result.new_messages.len(), 1);
        let crate::types::message::Message::User(supplement) = &result.new_messages[0] else {
            panic!("expected PDF user supplement");
        };
        assert!(!supplement.is_compact_summary);
        assert!(matches!(
            supplement.content.as_slice(),
            [crate::types::message::UserContent::MetaDocument { media_type, data }]
                if media_type == "application/pdf" && data == &pdf.base64
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pdf_pages_primary_and_image_supplement_matches_official_placement_when_available() {
        if !crate::utils::pdf::is_pdftoppm_available() {
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("cometix-read-pdf-pages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("doc.pdf");
        write_minimal_pdf(&path);
        let path_str = path.display().to_string();

        let result = match FileReadTool
            .call(
                &serde_json::json!({ "file_path": path_str, "pages": "1" }),
                &crate::tool::ToolUseContext::default(),
                None,
            )
            .await
        {
            Ok(output) => output,
            // Sandbox / missing session dir can block `pdftoppm` spawn or writes.
            Err(err)
                if err.message().contains("Operation not permitted")
                    || err.message().contains("pdftoppm") =>
            {
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
            Err(err) => panic!("pdf pages: {}", err.message()),
        };
        let ReadOutput::Parts(parts) = &result.data else {
            panic!("expected PdfParts, got {:?}", result.data);
        };
        assert_eq!(parts.count, 1);
        let mapped = FileReadTool::map_tool_result_to_tool_result_block_param(
            &result.data,
            result.memory_file_mtime_ms(),
            None,
        )
        .expect("PDF parts metadata mapper");
        assert_eq!(
            mapped.content,
            format!(
                "PDF pages extracted: 1 page(s) from {} ({})",
                parts.file_path,
                format_file_size(parts.original_size)
            )
        );
        assert!(mapped.content_blocks.is_empty());
        assert_eq!(result.new_messages.len(), 1);
        let crate::types::message::Message::User(supplement) = &result.new_messages[0] else {
            panic!("expected PDF-page user supplement");
        };
        assert!(matches!(
            supplement.content.as_slice(),
            [crate::types::message::UserContent::MetaImage { media_type, data }]
                if media_type.starts_with("image/") && !data.is_empty()
        ));
        let _ = std::fs::remove_dir_all(&parts.output_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pdf_order_filter_sort_and_concurrent_settlement_matches_official() {
        use base64::Engine as _;
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let mut restore = PdfFifoRestore {
            path: Some(EnvVarGuard::preserve("PATH")),
            read_task: None,
            prefix_marker: None,
            producer_cancel: None,
            fifos: Vec::new(),
            workers: Vec::new(),
        };

        let dir = std::env::temp_dir().join(format!(
            "cometix-read-pdf-order-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("fake poppler bin");
        let pdf_path = dir.join("doc.pdf");
        write_minimal_pdf(&pdf_path);

        let first_jpeg = dir.join("first.jpg");
        let second_jpeg = dir.join("second.jpg");
        let uppercase_jpeg = dir.join("ignored.JPG");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0])))
            .save_with_format(&first_jpeg, image::ImageFormat::Jpeg)
            .expect("first JPEG");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb([0, 0, 255])))
            .save_with_format(&second_jpeg, image::ImageFormat::Jpeg)
            .expect("second JPEG");
        std::fs::copy(&first_jpeg, &uppercase_jpeg).expect("uppercase JPEG");

        let prefix_marker = dir.join("prefix");
        restore.prefix_marker = Some(prefix_marker.clone());
        let producer_cancel = dir.join("cancel-producer");
        restore.producer_cancel = Some(producer_cancel.clone());
        let pdfinfo_marker = dir.join("pdfinfo-called");
        let pdftoppm = bin_dir.join("pdftoppm");
        std::fs::write(
            &pdftoppm,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"-v\" ]; then echo fake-poppler >&2; exit 0; fi\n\
                 if [ -e '{}' ]; then exit 1; fi\n\
                 for prefix in \"$@\"; do :; done\n\
                 mkfifo \"${{prefix}}-01.jpg\"\n\
                 mkfifo \"${{prefix}}-02.jpg\"\n\
                 python3 - '{}' \"${{prefix}}-01.jpg\" \"${{prefix}}-02.jpg\" >/dev/null 2>&1 <<'PY' &\n\
import os, sys, time\n\
time.sleep(2)\n\
if not os.path.exists(sys.argv[1]):\n\
    raise SystemExit\n\
for path in sys.argv[2:]:\n\
    try:\n\
        fd = os.open(path, os.O_WRONLY | os.O_NONBLOCK)\n\
    except OSError:\n\
        pass\n\
    else:\n\
        try:\n\
            os.write(fd, b'\\0')\n\
        finally:\n\
            os.close(fd)\n\
    try:\n\
        os.unlink(path)\n\
    except OSError:\n\
        pass\n\
PY\n\
                 if [ -e '{}' ]; then rm -f \"${{prefix}}-01.jpg\" \"${{prefix}}-02.jpg\"; exit 1; fi\n\
                 printf '%s' \"$prefix\" > '{}.tmp'\n\
                 mv '{}.tmp' '{}'\n\
                 cp '{}' \"${{prefix}}-00.JPG\"\n",
                producer_cancel.display(),
                producer_cancel.display(),
                producer_cancel.display(),
                prefix_marker.display(),
                prefix_marker.display(),
                prefix_marker.display(),
                uppercase_jpeg.display(),
            ),
        )
        .expect("pdftoppm script");
        std::fs::set_permissions(&pdftoppm, std::fs::Permissions::from_mode(0o755))
            .expect("pdftoppm executable");

        let pdfinfo = bin_dir.join("pdfinfo");
        std::fs::write(
            &pdfinfo,
            format!(
                "#!/bin/sh\nprintf called > '{}'\nprintf 'Pages: 11\\n'\n",
                pdfinfo_marker.display(),
            ),
        )
        .expect("pdfinfo script");
        std::fs::set_permissions(&pdfinfo, std::fs::Permissions::from_mode(0o755))
            .expect("pdfinfo executable");

        let process_env = crate::utils::process_env::snapshot();
        let old_path = process_env.var_os("PATH").unwrap_or_default();
        let mut test_path = vec![bin_dir.clone()];
        test_path.extend(std::env::split_paths(&old_path));
        crate::utils::process_env::set("PATH", std::env::join_paths(test_path).expect("test PATH"));
        crate::utils::pdf::reset_pdftoppm_cache();

        let pdf_path_string = pdf_path.display().to_string();
        let read_task = tokio::spawn(async move {
            FileReadTool
                .call(
                    &serde_json::json!({"file_path": pdf_path_string, "pages": "1-2"}),
                    &crate::tool::ToolUseContext::default(),
                    None,
                )
                .await
        });
        restore.read_task = Some(read_task.abort_handle());
        let marker_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !prefix_marker.exists() && std::time::Instant::now() < marker_deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let prefix = std::fs::read_to_string(&prefix_marker).expect("pdftoppm output prefix");
        let first_fifo = std::path::PathBuf::from(format!("{prefix}-01.jpg"));
        let second_fifo = std::path::PathBuf::from(format!("{prefix}-02.jpg"));
        restore.fifos = vec![first_fifo.clone(), second_fifo.clone()];
        let fifo_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (!first_fifo.exists() || !second_fifo.exists())
            && std::time::Instant::now() < fifo_deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            first_fifo.exists() && second_fifo.exists(),
            "FIFO creation timeout"
        );
        let first_bytes = std::fs::read(&first_jpeg).expect("first bytes");
        let second_bytes = std::fs::read(&second_jpeg).expect("second bytes");

        let (second_tx, second_rx) = std::sync::mpsc::channel();
        restore.workers.push(std::thread::spawn({
            let second_bytes = second_bytes.clone();
            move || {
                let result = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&second_fifo)
                    .and_then(|mut fifo| fifo.write_all(&second_bytes));
                let _ = second_tx.send(result);
            }
        }));
        let concurrent_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let second_completed_before_first = loop {
            match second_rx.try_recv() {
                Ok(result) => {
                    result.expect("second page FIFO write");
                    break true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("second page writer disconnected")
                }
                Err(std::sync::mpsc::TryRecvError::Empty)
                    if std::time::Instant::now() >= concurrent_deadline =>
                {
                    break false;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        };

        let (first_tx, first_rx) = std::sync::mpsc::channel();
        restore.workers.push(std::thread::spawn({
            let first_bytes = first_bytes.clone();
            move || {
                let result = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&first_fifo)
                    .and_then(|mut fifo| fifo.write_all(&first_bytes));
                let _ = first_tx.send(result);
            }
        }));
        let first_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match first_rx.try_recv() {
                Ok(result) => {
                    result.expect("first page FIFO write");
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("first page writer disconnected")
                }
                Err(std::sync::mpsc::TryRecvError::Empty)
                    if std::time::Instant::now() >= first_deadline =>
                {
                    panic!("first page should settle within the fixture deadline")
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
        if !second_completed_before_first {
            let settlement_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                match second_rx.try_recv() {
                    Ok(result) => {
                        result.expect("second page FIFO write");
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        panic!("second page writer disconnected")
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty)
                        if std::time::Instant::now() >= settlement_deadline =>
                    {
                        panic!("second page should settle after first")
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), read_task)
            .await
            .expect("PDF page read timeout")
            .expect("PDF page task")
            .expect("PDF page read");
        restore.read_task = None;
        assert!(
            second_completed_before_first,
            "all Promise.all page reads must start before the first page settles"
        );
        assert!(
            !pdfinfo_marker.exists(),
            "pages mode must enter extraction without a full-document page count"
        );
        let ReadOutput::Parts(parts) = &result.data else {
            panic!("expected PDF parts, got {:?}", result.data);
        };
        assert_eq!(parts.count, 2, "uppercase .JPG output must be ignored");
        let crate::types::message::Message::User(supplement) = &result.new_messages[0] else {
            panic!("expected page image supplement");
        };
        let encoded_first = base64::engine::general_purpose::STANDARD.encode(first_bytes);
        let encoded_second = base64::engine::general_purpose::STANDARD.encode(second_bytes);
        assert!(matches!(
            supplement.content.as_slice(),
            [
                crate::types::message::UserContent::MetaImage { data: first, .. },
                crate::types::message::UserContent::MetaImage { data: second, .. },
            ] if first == &encoded_first && second == &encoded_second
        ));

        let missing = dir.join("missing.pdf");
        let full_error = FileReadTool
            .call(
                &serde_json::json!({"file_path": missing.display().to_string()}),
                &crate::tool::ToolUseContext::default(),
                None,
            )
            .await
            .expect_err("page-count guard must run before the full-document stat");
        assert_eq!(
            full_error.message(),
            "This PDF has 11 pages, which is too many to read at once. Use the pages parameter to read specific page ranges (e.g., pages: \"1-5\"). Maximum 20 pages per request."
        );
        assert!(pdfinfo_marker.exists());

        drop(restore);
        let _ = std::fs::remove_dir_all(&parts.output_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cached_read_dedup_killswitch_config_cannot_disable_dedup() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        struct ConfigRestore(Option<crate::utils::config::GlobalConfig>);
        impl Drop for ConfigRestore {
            fn drop(&mut self) {
                crate::utils::config::replace_test_global_config(self.0.take());
            }
        }
        // Clear every analytics-disable condition so the injected disk cache
        // would be reachable if the killswitch were still read from config;
        // dedup must stay on because the switch table owns the answer.
        let _env_restore = [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "DISABLE_TELEMETRY",
        ]
        .into_iter()
        .map(EnvVarGuard::unset)
        .collect::<Vec<_>>();
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_read_dedup_killswitch".to_string(),
            serde_json::Value::Bool(true),
        )]));
        let _restore = ConfigRestore(crate::utils::config::replace_test_global_config(Some(
            config,
        )));
        let dir = std::env::temp_dir().join(format!(
            "cometix-read-dedup-killswitch-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("same.txt");
        std::fs::write(&path, "hello\n").unwrap();
        let mtime = crate::utils::file::get_file_modification_time_result(&path).unwrap();
        let path_str = path.display().to_string();
        let context = crate::tool::ToolUseContext {
            read_file_state: crate::tool::SharedFileStateCache::from_entries(vec![
                crate::utils::query_helpers::ReadFileStateEntry {
                    path: path_str.clone(),
                    content: Some("hello\n".to_string()),
                    timestamp_ms: Some(mtime),
                    offset: Some(serde_json::json!(1)),
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Read,
                },
            ]),
            ..crate::tool::ToolUseContext::default()
        };

        assert!(matches!(
            FileReadTool
                .call(&serde_json::json!({"file_path": path_str}), &context, None)
                .await
                .unwrap()
                .data,
            ReadOutput::FileUnchanged(_)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn dedup_returns_file_unchanged_when_mtime_matches() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        struct ConfigRestore(Option<crate::utils::config::GlobalConfig>);
        impl Drop for ConfigRestore {
            fn drop(&mut self) {
                crate::utils::config::replace_test_global_config(self.0.take());
            }
        }
        let _restore = ConfigRestore(crate::utils::config::replace_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        )));
        let dir = std::env::temp_dir().join(format!("cometix-read-dedup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("same.txt");
        std::fs::write(&path, "hello\n").expect("write");
        let mtime = crate::utils::file::get_file_modification_time_result(&path).expect("mtime");
        let path_str = path.display().to_string();

        let mut context = crate::tool::ToolUseContext::default();
        context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: path_str.clone(),
                content: Some("hello\n".to_string()),
                timestamp_ms: Some(mtime),
                offset: Some(serde_json::json!(1)),
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });

        let output = FileReadTool
            .call(
                &serde_json::json!({ "file_path": path_str }),
                &context,
                None,
            )
            .await
            .expect("dedup");
        match output.data {
            ReadOutput::FileUnchanged(unchanged) => {
                assert_eq!(unchanged.file_path, path.display().to_string());
            }
            _ => panic!("expected FileUnchanged"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
