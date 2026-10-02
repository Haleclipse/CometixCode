//! UI-only port of official `tools/FileReadTool/UI.tsx`.

use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderSegment, ToolRenderTone,
};
use crate::types::message::{ReadResultKind, ToolResultStatus};
use crate::utils::file::get_display_path;
use crate::utils::format::format_file_size;
use std::path::{Path, PathBuf};

// Mechanical serde_json carriers for the JavaScript operators written inline
// in CC `tools/FileReadTool/UI.tsx:35-71` `renderToolUseMessage`. They remain
// in the UI owner and carry no Read call policy. ECMAScript Number
// stringification delegates directly to `ryu-js`.
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

/// Maps to: CC `tools/FileReadTool/UI.tsx#userFacingName`.
pub fn user_facing_name(input: Option<&serde_json::Value>) -> String {
    user_facing_name_for_config_home(input, crate::utils::env_utils::get_claude_config_home_dir())
}

/// Maps to: CC `tools/FileReadTool/UI.tsx#getToolUseSummary` (:177-188).
pub fn get_tool_use_summary(input: Option<&serde_json::Value>) -> Option<String> {
    let file_path = input.and_then(|input| input.get("file_path"))?.as_str()?;
    if file_path.is_empty() {
        return None;
    }
    get_agent_output_task_id(file_path).or_else(|| Some(get_display_path(file_path)))
}

fn user_facing_name_for_config_home(
    input: Option<&serde_json::Value>,
    config_home: PathBuf,
) -> String {
    let Some(file_path) = input
        .and_then(|input| input.get("file_path"))
        .and_then(serde_json::Value::as_str)
    else {
        return "Read".to_string();
    };
    if is_plan_path(&file_path, config_home) {
        return "Reading Plan".to_string();
    }
    if get_agent_output_task_id(&file_path).is_some() {
        return "Read agent output".to_string();
    }
    "Read".to_string()
}

fn is_plan_path(file_path: &str, config_home: PathBuf) -> bool {
    file_path.starts_with(&config_home.join("plans").display().to_string())
}

/// Maps to: CC `tools/FileReadTool/UI.tsx:35-71` `renderToolUseMessage`,
/// including JavaScript truthiness/nullish/arithmetic for authoritative raw
/// replacements that reach this function after the initial strict parse.
pub fn render_tool_use_message(input: &serde_json::Value, verbose: bool) -> Option<String> {
    let file_path = input.get("file_path")?.as_str()?;
    if file_path.is_empty() {
        return None;
    }
    if get_agent_output_task_id(file_path).is_some() {
        return Some(String::new());
    }

    let display_path = if verbose {
        file_path.to_string()
    } else {
        get_display_path(file_path)
    };
    if let Some(pages) = input.get("pages").filter(|pages| javascript_truthy(pages)) {
        return Some(format!(
            "{display_path} · pages {}",
            javascript_to_string(pages)
        ));
    }
    let offset = input.get("offset");
    let limit = input.get("limit");
    if verbose && (offset.is_some_and(javascript_truthy) || limit.is_some_and(javascript_truthy)) {
        let default_start = serde_json::json!(1);
        let start_line = match offset {
            None | Some(serde_json::Value::Null) => &default_start,
            Some(offset) => offset,
        };
        let line_range = match limit.filter(|limit| javascript_truthy(limit)) {
            Some(limit) => {
                // CC evaluates `startLine + limit - 1`: `+` may concatenate
                // after ToPrimitive, then `-` coerces that result to Number.
                let string_addition = matches!(
                    start_line,
                    serde_json::Value::String(_)
                        | serde_json::Value::Array(_)
                        | serde_json::Value::Object(_)
                ) || matches!(
                    limit,
                    serde_json::Value::String(_)
                        | serde_json::Value::Array(_)
                        | serde_json::Value::Object(_)
                );
                let sum = if string_addition {
                    javascript_string_to_number(&format!(
                        "{}{}",
                        javascript_to_string(start_line),
                        javascript_to_string(limit)
                    ))
                } else {
                    javascript_to_number(start_line) + javascript_to_number(limit)
                };
                format!(
                    "lines {}-{}",
                    javascript_to_string(start_line),
                    ryu_js::Buffer::new().format(sum - 1.0).to_string()
                )
            }
            None => format!("from line {}", javascript_to_string(start_line)),
        };
        return Some(format!("{display_path} · {line_range}"));
    }
    Some(display_path)
}

/// L1 React/Ink → iocraft projection of the `FilePathLink` child returned by
/// CC `tools/FileReadTool/UI.tsx:35-71` `renderToolUseMessage`. Read UI keeps
/// path-label/suffix ownership; the generic assistant row only places it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadToolUsePathLink {
    pub file_path: String,
    pub label: String,
    pub suffix: String,
}

/// Maps to: CC `tools/FileReadTool/UI.tsx:35-71` `renderToolUseMessage`'s
/// linked-path representation.
pub fn render_tool_use_path_link(
    input: &serde_json::Value,
    verbose: bool,
    rendered_description: &str,
) -> Option<ReadToolUsePathLink> {
    if rendered_description.is_empty() {
        return None;
    }
    let file_path = input.get("file_path")?.as_str()?;
    let label = if verbose {
        file_path.to_string()
    } else {
        get_display_path(file_path)
    };
    let suffix = rendered_description
        .strip_prefix(&label)
        .unwrap_or_default()
        .to_string();
    Some(ReadToolUsePathLink {
        file_path: file_path.to_string(),
        label,
        suffix,
    })
}

/// Maps to: CC `tools/FileReadTool/UI.tsx:73-83#renderToolUseTag`.
/// The tag projection stays in the Read UI owner; AssistantToolUseMessage only
/// places the tool-supplied tag next to the generic row.
pub fn render_tool_use_tag(input: &serde_json::Value) -> Option<String> {
    let file_path = input.get("file_path")?.as_str()?;
    get_agent_output_task_id(file_path)
}

/// Maps to: CC `tools/FileReadTool/UI.tsx#getAgentOutputTaskId` (:18-37).
/// Only the session task-output root is special; a repository path merely
/// containing `/tasks/<id>.output` must remain an ordinary Read.
fn get_agent_output_task_id(file_path: &str) -> Option<String> {
    let output_dir = crate::utils::task::disk_output::get_task_output_dir();
    let path = Path::new(file_path);
    let relative = path.strip_prefix(&output_dir).ok()?;
    if relative.components().count() != 1 {
        return None;
    }
    let filename = relative.file_name()?.to_str()?;
    let task_id = filename.strip_suffix(".output")?;
    let valid = !task_id.is_empty()
        && task_id.len() <= 20
        && task_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    valid.then(|| task_id.to_string())
}

/// Maps to: CC `tools/FileReadTool/UI.tsx#renderToolResultMessage`.
pub fn render_tool_result_lines(
    path: Option<&str>,
    line_count: Option<usize>,
    kind: ReadResultKind,
    original_size: Option<u64>,
    cell_count: Option<usize>,
    page_count: Option<usize>,
    output: Option<&super::Output>,
    status: ToolResultStatus,
    fallback: &str,
) -> Vec<ToolRenderLine> {
    match status {
        ToolResultStatus::Success => {
            // Maps to: CC `FileReadTool/UI.tsx` `renderToolResultMessage` — success
            // summaries use default Ink `<Text>` (not `color="success"`); only the
            // notebook-empty and unchanged branches set error/dim.
            if let Some(output) = output {
                use super::Output;
                let tone = match output {
                    Output::Notebook { cells, .. } if cells.is_empty() => ToolRenderTone::Error,
                    Output::FileUnchanged { .. } => ToolRenderTone::Inactive,
                    _ => ToolRenderTone::Normal,
                };
                let summary = summary_from_output(output);
                let segments = match output {
                    Output::Text { num_lines, .. } => Some(vec![
                        ToolRenderSegment::new("Read "),
                        ToolRenderSegment::new(num_lines.to_string()).with_bold(true),
                        ToolRenderSegment::new(format!(
                            " {}",
                            if number_is_one(num_lines) {
                                "line"
                            } else {
                                "lines"
                            }
                        )),
                    ]),
                    Output::Notebook { cells, .. } if !cells.is_empty() => Some(vec![
                        ToolRenderSegment::new("Read "),
                        ToolRenderSegment::new(cells.len().to_string()).with_bold(true),
                        ToolRenderSegment::new(" cells"),
                    ]),
                    Output::Parts {
                        original_size,
                        count,
                        ..
                    } => Some(vec![
                        ToolRenderSegment::new("Read "),
                        ToolRenderSegment::new(count.to_string()).with_bold(true),
                        ToolRenderSegment::new(format!(
                            " {} ({})",
                            if number_is_one(count) {
                                "page"
                            } else {
                                "pages"
                            },
                            format_file_size(number_to_u64(original_size))
                        )),
                    ]),
                    _ => None,
                };
                let mut line = ToolRenderLine::new(summary, tone);
                if let Some(segments) = segments {
                    line = line.with_segments(segments);
                }
                return vec![line];
            }
            let mut tone = ToolRenderTone::Normal;
            let summary = match kind {
                ReadResultKind::Text => match line_count {
                    Some(count) => format!("Read {count} {}", plural(count, "line", "lines")),
                    None => "Read file".to_string(),
                },
                ReadResultKind::Image => match original_size {
                    Some(size) => format!("Read image ({})", format_file_size(size)),
                    None => "Read image".to_string(),
                },
                ReadResultKind::Notebook => match cell_count {
                    Some(0) => {
                        tone = ToolRenderTone::Error;
                        "No cells found in notebook".to_string()
                    }
                    Some(count) => format!("Read {count} cells"),
                    None => "Read notebook".to_string(),
                },
                ReadResultKind::Pdf => match original_size {
                    Some(size) => format!("Read PDF ({})", format_file_size(size)),
                    None => "Read PDF".to_string(),
                },
                ReadResultKind::Parts => {
                    let count = page_count.unwrap_or(0);
                    match original_size {
                        Some(size) => format!(
                            "Read {count} {} ({})",
                            plural(count, "page", "pages"),
                            format_file_size(size)
                        ),
                        None => format!("Read {count} {}", plural(count, "page", "pages")),
                    }
                }
                ReadResultKind::FileUnchanged => {
                    tone = ToolRenderTone::Inactive;
                    "Unchanged since last read".to_string()
                }
                ReadResultKind::Unknown => match line_count {
                    Some(count) => format!("Read {count} {}", plural(count, "line", "lines")),
                    None => read_placeholder(path),
                },
            };
            let segments = match kind {
                ReadResultKind::Text => line_count.map(|count| {
                    vec![
                        ToolRenderSegment::new("Read "),
                        ToolRenderSegment::new(count.to_string()).with_bold(true),
                        ToolRenderSegment::new(format!(" {}", plural(count, "line", "lines"))),
                    ]
                }),
                ReadResultKind::Notebook => cell_count.filter(|count| *count > 0).map(|count| {
                    vec![
                        ToolRenderSegment::new("Read "),
                        ToolRenderSegment::new(count.to_string()).with_bold(true),
                        ToolRenderSegment::new(" cells"),
                    ]
                }),
                ReadResultKind::Parts => page_count.map(|count| {
                    let suffix = match original_size {
                        Some(size) => format!(
                            " {} ({})",
                            plural(count, "page", "pages"),
                            format_file_size(size)
                        ),
                        None => format!(" {}", plural(count, "page", "pages")),
                    };
                    vec![
                        ToolRenderSegment::new("Read "),
                        ToolRenderSegment::new(count.to_string()).with_bold(true),
                        ToolRenderSegment::new(suffix),
                    ]
                }),
                _ => None,
            };
            let mut line = ToolRenderLine::new(summary, tone);
            if let Some(segments) = segments {
                line = line.with_segments(segments);
            }
            vec![line]
        }
        ToolResultStatus::Error => vec![ToolRenderLine::new(
            read_error_text(fallback, "Error reading file"),
            ToolRenderTone::Error,
        )],
        ToolResultStatus::Rejected => vec![ToolRenderLine::new(
            read_error_text(fallback, "Tool use rejected"),
            ToolRenderTone::Warning,
        )],
        ToolResultStatus::Canceled => vec![ToolRenderLine::new(
            read_error_text(fallback, "Interrupted by user"),
            ToolRenderTone::Inactive,
        )],
    }
}

/// Maps to: CC `tools/FileReadTool/UI.tsx#renderToolUseErrorMessage`.
pub fn render_tool_use_error_message(result: &str, verbose: bool) -> Option<&'static str> {
    if verbose {
        return None;
    }
    if result.contains(crate::utils::file::FILE_NOT_FOUND_CWD_NOTE) {
        return Some("File not found");
    }
    crate::utils::messages::extract_tag(result, "tool_use_error")
        .is_some()
        .then_some("Error reading file")
}

fn read_placeholder(path: Option<&str>) -> String {
    let ext = path
        .and_then(|path| path.rsplit('.').next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => "Read image".to_string(),
        "pdf" => "Read PDF".to_string(),
        "ipynb" => "Read notebook".to_string(),
        _ => "Read file".to_string(),
    }
}

fn read_error_text(fallback: &str, default: &str) -> String {
    let trimmed = fallback.trim();
    if trimmed.is_empty() {
        return default.to_string();
    }
    if trimmed.contains("Note: your current working directory is") {
        return "File not found".to_string();
    }
    trimmed
        .strip_prefix("Error: ")
        .unwrap_or(trimmed)
        .to_string()
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

// ─── Recorded tool-result display projection ─────────────────────────────
// Maps to: CC `tools/FileReadTool/UI.tsx:85-148`. The canonical outputSchema
// parser lives in the sibling FileReadTool owner (`mod.rs`).

fn number_to_usize(number: &serde_json::Number) -> usize {
    number
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .or_else(|| {
            number
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0)
                .map(|value| value.trunc().min(usize::MAX as f64) as usize)
        })
        .unwrap_or_default()
}

fn number_is_one(number: &serde_json::Number) -> bool {
    number.as_f64() == Some(1.0)
}

fn number_to_u64(number: &serde_json::Number) -> u64 {
    number
        .as_u64()
        .or_else(|| {
            number
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0)
                .map(|value| value.trunc().min(u64::MAX as f64) as u64)
        })
        .unwrap_or_default()
}

pub(crate) fn summary_from_output(output: &super::Output) -> String {
    use super::Output;
    match output {
        Output::Text { num_lines, .. } => format!(
            "Read {num_lines} {}",
            if number_is_one(num_lines) {
                "line"
            } else {
                "lines"
            }
        ),
        Output::Image { original_size, .. } => {
            format!(
                "Read image ({})",
                format_file_size(number_to_u64(original_size))
            )
        }
        Output::Notebook { cells, .. } if cells.is_empty() => {
            "No cells found in notebook".to_string()
        }
        Output::Notebook { cells, .. } => {
            format!("Read {} cells", cells.len())
        }
        Output::Pdf { original_size, .. } => {
            format!(
                "Read PDF ({})",
                format_file_size(number_to_u64(original_size))
            )
        }
        Output::Parts {
            original_size,
            count,
            ..
        } => format!(
            "Read {count} {} ({})",
            if number_is_one(count) {
                "page"
            } else {
                "pages"
            },
            format_file_size(number_to_u64(original_size))
        ),
        Output::FileUnchanged { .. } => "Unchanged since last read".to_string(),
    }
}

/// The six render inputs `renderToolResultMessage` reads off a parsed output.
type ReadRenderFields = (
    Option<String>,
    Option<usize>,
    crate::types::message::ReadResultKind,
    Option<u64>,
    Option<usize>,
    Option<usize>,
);

fn render_fields_from_output(output: &super::Output) -> ReadRenderFields {
    use super::Output;
    use crate::types::message::ReadResultKind;
    match output {
        Output::Text {
            file_path,
            num_lines,
            ..
        } => (
            Some(file_path.clone()),
            Some(number_to_usize(num_lines)),
            ReadResultKind::Text,
            None,
            None,
            None,
        ),
        Output::Image { original_size, .. } => (
            None,
            None,
            ReadResultKind::Image,
            Some(number_to_u64(original_size)),
            None,
            None,
        ),
        Output::Notebook { file_path, cells } => (
            Some(file_path.clone()),
            None,
            ReadResultKind::Notebook,
            None,
            Some(cells.len()),
            None,
        ),
        Output::Pdf {
            file_path,
            original_size,
            ..
        } => (
            Some(file_path.clone()),
            None,
            ReadResultKind::Pdf,
            Some(number_to_u64(original_size)),
            None,
            None,
        ),
        Output::Parts {
            file_path,
            original_size,
            count,
            ..
        } => (
            Some(file_path.clone()),
            None,
            ReadResultKind::Parts,
            Some(number_to_u64(original_size)),
            None,
            Some(number_to_usize(count)),
        ),
        Output::FileUnchanged { file_path } => (
            Some(file_path.clone()),
            None,
            ReadResultKind::FileUnchanged,
            None,
            None,
            None,
        ),
    }
}

/// Maps to: CC `UserToolSuccessMessage.tsx:80-96` as it applies to FileReadTool
/// — parse the raw `toolUseResult` with the tool's own output schema, render
/// from the parsed value, and render nothing when it does not parse.
///
/// This is the by-tool-name entry the dispatch calls — the only Read
/// render path now that the Read display variants are gone.
pub(crate) fn render_tool_result_message(
    raw_output: Option<&serde_json::Value>,
    status: ToolResultStatus,
    fallback: &str,
) -> Vec<ToolRenderLine> {
    // CC bails on a missing `toolUseResult` before touching the tool
    // (`UserToolSuccessMessage.tsx:72`).
    let Some(raw_output) = raw_output else {
        return Vec::new();
    };
    let raw_output = super::javascript_runtime_value(raw_output);
    // CC: `safeParse` failure returns null, i.e. the row renders nothing. The
    // Rust equivalent used to be minting `ReadSuppressed`, which renders empty.
    let Some(output) = super::parse_output(&raw_output) else {
        return Vec::new();
    };
    let (path, line_count, kind, original_size, cell_count, page_count) =
        render_fields_from_output(&output);
    render_tool_result_lines(
        path.as_deref(),
        line_count,
        kind,
        original_size,
        cell_count,
        page_count,
        Some(&output),
        status,
        fallback,
    )
}

#[cfg(test)]
mod tests {
    use super::super::parse_output;
    use super::*;
    use serde_json::json;

    /// Maps to: CC `FileReadTool/UI.tsx#renderToolResultMessage` output per
    /// wire shape — the raw entry is now the only Read render path.
    #[test]
    fn render_tool_result_message_matches_official_summaries() {
        let render = |raw: serde_json::Value| {
            render_tool_result_message(Some(&raw), ToolResultStatus::Success, "fallback")
        };
        let text = render(serde_json::json!({
            "type": "text",
            "file": {"filePath": "src/main.rs", "content": "fn main() {}", "numLines": 1, "startLine": 1, "totalLines": 1}
        }));
        assert_eq!(text[0].text, "Read 1 line");
        let notebook = render(serde_json::json!({
            "type": "notebook",
            "file": {"filePath": "nb.ipynb", "cells": []}
        }));
        assert_eq!(notebook[0].text, "No cells found in notebook");
        let image = render(serde_json::json!({
            "type": "image",
            "file": {"base64": "AA==", "type": "image/png", "originalSize": 4096}
        }));
        assert_eq!(image[0].text, "Read image (4KB)");
    }

    /// CC returns null when `toolUseResult` is absent or fails `safeParse`
    /// (`UserToolSuccessMessage.tsx:72,81`), i.e. the row renders nothing.
    #[test]
    fn raw_entry_renders_nothing_when_output_is_absent_or_unparseable() {
        assert!(render_tool_result_message(None, ToolResultStatus::Success, "fallback").is_empty());
        let garbage = json!({"type": "not-a-read-result"});
        assert!(
            render_tool_result_message(Some(&garbage), ToolResultStatus::Success, "fallback")
                .is_empty()
        );
    }

    #[test]
    fn read_tool_use_message_matches_official_pages_and_verbose_ranges() {
        assert_eq!(
            render_tool_use_message(&json!({"file_path": "manual.pdf", "pages": "2-4"}), false),
            Some("manual.pdf · pages 2-4".to_string())
        );
        assert_eq!(
            render_tool_use_message(
                &json!({"file_path": "src/main.rs", "offset": 10, "limit": 5}),
                true
            ),
            Some("src/main.rs · lines 10-14".to_string())
        );
        assert_eq!(
            render_tool_use_message(&json!({"file_path": "src/main.rs", "offset": 10}), true),
            Some("src/main.rs · from line 10".to_string())
        );
    }

    #[test]
    fn raw_read_tool_use_ranges_match_official_javascript_operators() {
        for (offset, limit, expected) in [
            (json!("2"), Some(json!("1")), "src/main.rs · lines 2-20"),
            (json!(1.5), Some(json!(2.5)), "src/main.rs · lines 1.5-3"),
            (json!(-1), Some(json!(1)), "src/main.rs · lines -1--1"),
            (json!(null), Some(json!(1)), "src/main.rs · lines 1-1"),
            (json!(true), Some(json!(1)), "src/main.rs · lines true-1"),
            (json!([]), Some(json!(1)), "src/main.rs · lines -0"),
            (json!([2]), Some(json!(1)), "src/main.rs · lines 2-20"),
            (json!("abc"), Some(json!(1)), "src/main.rs · lines abc-NaN"),
            (
                json!("\u{0085}"),
                Some(json!(1)),
                "src/main.rs · lines \u{0085}-NaN",
            ),
        ] {
            let mut input = json!({"file_path": "src/main.rs", "offset": offset});
            if let Some(limit) = limit {
                input
                    .as_object_mut()
                    .unwrap()
                    .insert("limit".to_string(), limit);
            }
            assert_eq!(
                render_tool_use_message(&input, true).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(
            render_tool_use_message(&json!({"file_path": "manual.pdf", "pages": []}), false),
            Some("manual.pdf · pages ".to_string())
        );
        assert_eq!(
            render_tool_use_message(&json!({"file_path": "src/main.rs", "offset": 0}), true),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn read_tool_use_message_and_tag_keep_agent_output_ownership_in_read_ui() {
        let path = crate::utils::task::disk_output::get_task_output_dir()
            .join("task-abc.output")
            .display()
            .to_string();
        let input = json!({"file_path": path});
        assert_eq!(render_tool_use_message(&input, false), Some(String::new()));
        assert_eq!(render_tool_use_tag(&input), Some("task-abc".to_string()));
    }

    #[test]
    fn read_user_facing_name_matches_official_plan_and_agent_output_cases() {
        let config_home = PathBuf::from("/tmp/cometix-config");
        let plan_path = config_home.join("plans/plan.md");
        assert_eq!(
            user_facing_name_for_config_home(
                Some(&json!({"file_path": plan_path.to_string_lossy().to_string()})),
                config_home.clone()
            ),
            "Reading Plan"
        );
        let task_output = crate::utils::task::disk_output::get_task_output_dir()
            .join("task-abc.output")
            .display()
            .to_string();
        assert_eq!(
            user_facing_name(Some(&json!({"file_path": task_output}))),
            "Read agent output"
        );
        assert_eq!(
            user_facing_name(Some(&json!({"file_path": "src/main.rs"}))),
            "Read"
        );
        assert_eq!(
            user_facing_name(Some(
                &json!({"file_path": "/repo/tasks/not-a-session.output"})
            )),
            "Read"
        );
        for alias in [
            json!({"filePath": "src/main.rs"}),
            json!({"path": "src/main.rs"}),
        ] {
            assert_eq!(user_facing_name(Some(&alias)), "Read");
            assert_eq!(render_tool_use_message(&alias, false), None);
            assert_eq!(render_tool_use_tag(&alias), None);
        }
    }

    #[test]
    fn read_error_renderer_decision_matches_official_non_verbose_cases() {
        let missing = "File does not exist. Note: your current working directory is /tmp.";
        assert_eq!(
            render_tool_use_error_message(missing, false),
            Some("File not found")
        );
        assert_eq!(render_tool_use_error_message(missing, true), None);
        assert_eq!(
            render_tool_use_error_message("<tool_use_error>boom</tool_use_error>", false,),
            Some("Error reading file")
        );
        assert_eq!(
            render_tool_use_error_message("<tool_use_error>boom</tool_use_error>", true),
            None
        );
        assert_eq!(render_tool_use_error_message("disk exploded", false), None);
    }

    #[test]
    fn read_success_rows_bold_only_the_official_line_cell_and_page_counts() {
        for (kind, line_count, cell_count, page_count, expected) in [
            (ReadResultKind::Text, Some(2), None, None, "2"),
            (ReadResultKind::Notebook, None, Some(1), None, "1"),
            (ReadResultKind::Parts, None, None, Some(3), "3"),
        ] {
            let lines = render_tool_result_lines(
                None,
                line_count,
                kind,
                Some(4096),
                cell_count,
                page_count,
                None,
                ToolResultStatus::Success,
                "",
            );
            assert_eq!(lines[0].segments.len(), 3);
            assert_eq!(lines[0].segments[1].text, expected);
            assert!(lines[0].segments[1].bold);
        }
    }

    #[test]
    fn recovered_fractional_count_renders_without_usize_truncation() {
        let output = parse_output(&json!({
            "type": "text",
            "file": {
                "filePath": "src/main.rs",
                "content": "x",
                "numLines": 1.5,
                "startLine": 1,
                "totalLines": 1.5
            }
        }))
        .unwrap();
        let lines = render_tool_result_lines(
            Some("src/main.rs"),
            Some(1),
            ReadResultKind::Text,
            None,
            None,
            None,
            Some(&output),
            ToolResultStatus::Success,
            "",
        );
        assert_eq!(lines[0].text, "Read 1.5 lines");
        assert_eq!(lines[0].segments[1].text, "1.5");
    }

    #[test]
    fn read_output_parser_is_schema_driven_and_keeps_unrestricted_zod_numbers() {
        let output = parse_output(&json!({
            "type": "text",
            "file": {
                "filePath": "src/main.rs",
                "content": "fn main() {}",
                "numLines": 1.5,
                "startLine": -2,
                "totalLines": 3e2
            }
        }))
        .expect("valid Zod number fields");
        let crate::tools::file_read_tool::Output::Text {
            num_lines,
            start_line,
            total_lines,
            ..
        } = output
        else {
            panic!("expected text output")
        };
        assert_eq!(num_lines.to_string(), "1.5");
        assert_eq!(start_line.to_string(), "-2");
        assert_eq!(total_lines.to_string(), "300");
        let rounded = parse_output(&json!({
            "type": "text",
            "file": {
                "filePath": "src/main.rs",
                "content": "x",
                "numLines": 1,
                "startLine": 9_007_199_254_740_993_u64,
                "totalLines": 1
            }
        }))
        .expect("JavaScript Number-rounded fields remain valid");
        assert!(matches!(
            rounded,
            crate::tools::file_read_tool::Output::Text { start_line, .. }
                if start_line.to_string() == "9007199254740992"
        ));
        assert!(
            parse_output(&json!({
                "type": "text",
                "file": {"filePath": "x", "content": "x", "numLines": 1, "startLine": 1}
            }))
            .is_none()
        );
        assert!(
            parse_output(&json!({
                "type": "image",
                "file": {"base64": "x", "type": "image/bmp", "originalSize": 1}
            }))
            .is_none()
        );
        let notebook = parse_output(&json!({
            "type": "notebook",
            "file": {
                "filePath": "odd.ipynb",
                "cells": [null, 7, {"schema": "allows any cell"}]
            }
        }))
        .expect("notebook cells are z.any()");
        assert!(matches!(
            notebook,
            crate::tools::file_read_tool::Output::Notebook { cells, .. }
                if cells == vec![json!(null), json!(7), json!({"schema": "allows any cell"})]
        ));
    }
}
