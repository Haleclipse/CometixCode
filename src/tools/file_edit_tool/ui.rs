//! UI-only port of official `tools/FileEditTool/UI.tsx` result rendering.
//! Official FileEdit delegates successful updates to
//! `FileEditToolUpdatedMessage` → `StructuredDiffList`. This Rust main-screen
//! port renders already-recorded structured patch hunks only; it never reads
//! files, writes files, or computes patches at render time.

use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderTone,
};
use crate::components::structured_diff;
use crate::types::message::{StructuredDiffHunk, ToolResultStatus};
use crate::utils::file::get_display_path;
use crate::utils::plans::get_plans_directory;

/// Maps to CC `tools/FileEditTool/UI.tsx#RejectionDiffData`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RejectionDiffData {
    pub(crate) patch: Vec<StructuredDiffHunk>,
    pub(crate) first_line: Option<String>,
    pub(crate) file_content: Option<String>,
}

/// Maps to CC `tools/FileEditTool/UI.tsx#loadRejectionDiff`.
pub(crate) fn load_rejection_diff(
    file_path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> RejectionDiffData {
    let load = || -> Result<RejectionDiffData, String> {
        let context = crate::utils::read_edit_context::read_edit_context(
            std::path::Path::new(file_path),
            old_string,
            crate::utils::diff::CONTEXT_LINES,
        )
        .map_err(|error| error.to_string())?;
        let Some(context) = context else {
            return rejection_diff_from_inputs(old_string, new_string);
        };
        if context.truncated || context.content.is_empty() {
            return rejection_diff_from_inputs(old_string, new_string);
        }
        let actual_old = super::utils::find_actual_string(&context.content, old_string)
            .unwrap_or_else(|| old_string.to_string());
        let actual_new = super::utils::preserve_quote_style(old_string, &actual_old, new_string);
        let (local_patch, _) = super::utils::get_patch_for_edit(
            &context.content,
            &actual_old,
            &actual_new,
            replace_all,
        )?;
        let first_line = (context.line_offset == 1).then(|| {
            context
                .content
                .split_once('\n')
                .map_or(context.content.as_str(), |(first, _)| first)
                .to_string()
        });
        Ok(RejectionDiffData {
            patch: crate::utils::diff::adjust_hunk_line_numbers(
                &local_patch,
                context.line_offset.saturating_sub(1) as isize,
            ),
            first_line,
            file_content: Some(context.content),
        })
    };

    load().unwrap_or_else(|error| {
        // The user may have manually applied the edit while the rejection diff
        // was being prepared. CC logs and renders text-only in this case.
        crate::utils::debug::log_for_debugging(&format!(
            "Unable to load rejected Edit diff: {error}"
        ));
        RejectionDiffData::default()
    })
}

/// Maps to: CC `tools/FileEditTool/UI.tsx:89-108` `renderToolResultMessage`
/// — unconditional `FileEditToolUpdatedMessage`, even for an empty patch.
/// Element-pipeline owner; the line pipeline lives in
/// `render_tool_result_lines`.
pub(crate) fn render_tool_result_message(
    output: &crate::tools::file_edit_tool::types::FileEditOutput,
    verbose: bool,
    style: Option<&str>,
) -> iocraft::AnyElement<'static> {
    use iocraft::prelude::*;
    let preview_hint = plans_preview_hint(&output.file_path);
    let first_line = first_line_of(&output.original_file);
    element! {
        crate::components::file_edit_tool_updated_message::FileEditToolUpdatedMessage(
            file_path: output.file_path.clone(),
            structured_patch: output.structured_patch.clone(),
            first_line: Some(first_line),
            file_content: Some(output.original_file.clone()),
            verbose: verbose,
            style: style.map(str::to_string),
            preview_hint: preview_hint,
        )
    }
    .into_any()
}

/// Maps to: CC `tools/FileEditTool/UI.tsx:110-171`
/// `renderToolUseRejectedMessage` — entirely input-driven. The preview diff
/// is computed synchronously at render: an L1 architectural deviation from
/// CC's Suspense-deferred load, recorded in PORTING.md under the rejected
/// `Suspense-deferred render body carrier` proposal.
pub(crate) fn render_tool_use_rejected_message(
    input: &serde_json::Value,
    verbose: bool,
    style: Option<&str>,
) -> Option<iocraft::AnyElement<'static>> {
    use crate::components::file_edit_tool_use_rejected_message::FileEditToolUseRejectedMessage;
    use iocraft::prelude::*;
    let file_path = input.get("file_path").and_then(serde_json::Value::as_str)?;
    let old_string = input
        .get("old_string")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let new_string = input
        .get("new_string")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let replace_all = input
        .get("replace_all")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let style_owned = style.map(str::to_string);
    // Defensive hashline shape (:135-144): plain update rejection row.
    if input.get("edits").is_some_and(|edits| !edits.is_null()) {
        return Some(
            element! {
                FileEditToolUseRejectedMessage(
                    file_path: file_path.to_string(),
                    operation: "update".to_string(),
                    verbose: verbose,
                    style: style_owned.clone(),
                )
            }
            .into_any(),
        );
    }
    // New-file creation shows a content preview (:149-159).
    if old_string.is_empty() {
        return Some(
            element! {
                FileEditToolUseRejectedMessage(
                    file_path: file_path.to_string(),
                    operation: "write".to_string(),
                    content: Some(new_string.to_string()),
                    first_line: Some(first_line_of(new_string)),
                    verbose: verbose,
                    style: style_owned.clone(),
                )
            }
            .into_any(),
        );
    }
    let data = load_rejection_diff(file_path, old_string, new_string, replace_all);
    Some(
        element! {
            FileEditToolUseRejectedMessage(
                file_path: file_path.to_string(),
                operation: "update".to_string(),
                patch: (!data.patch.is_empty()).then(|| data.patch.clone()),
                first_line: data.first_line.clone(),
                file_content: data.file_content.clone(),
                verbose: verbose,
                style: style_owned.clone(),
            )
        }
        .into_any(),
    )
}

/// The `/plan to preview` hint for plan-directory paths, shared by the
/// success and rejection arms (CC keeps it inline per arm).
pub(crate) fn plans_preview_hint(path: &str) -> Option<String> {
    path.starts_with(&get_plans_directory().display().to_string())
        .then(|| "/plan to preview".to_string())
}

fn first_line_of(content: &str) -> String {
    content
        .split_once('\n')
        .map_or(content, |(first, _)| first)
        .to_string()
}

fn rejection_diff_from_inputs(
    old_string: &str,
    new_string: &str,
) -> Result<RejectionDiffData, String> {
    let (patch, _) = super::utils::get_patch_for_edit(old_string, old_string, new_string, false)?;
    Ok(RejectionDiffData {
        patch,
        first_line: None,
        file_content: None,
    })
}

/// Maps to: CC `tools/FileEditTool/UI.tsx#userFacingName`.
pub fn user_facing_name(input: Option<&serde_json::Value>) -> String {
    let Some(input) = input else {
        return "Update".to_string();
    };
    if let Some(file_path) = input.get("file_path").and_then(|value| value.as_str()) {
        let plans = get_plans_directory();
        if file_path.starts_with(&plans.display().to_string())
            || std::path::Path::new(file_path).starts_with(&plans)
        {
            return "Updated plan".to_string();
        }
    }
    if input.get("edits").is_some_and(|edits| !edits.is_null()) {
        return "Update".to_string();
    }
    let old_string = input
        .get("old_string")
        .or_else(|| input.get("oldString"))
        .and_then(|value| value.as_str());
    if old_string == Some("") {
        "Create".to_string()
    } else {
        "Update".to_string()
    }
}

/// Maps to: CC `tools/FileEditTool/UI.tsx#getToolUseSummary`.
pub fn get_tool_use_summary(input: Option<&serde_json::Value>) -> Option<String> {
    let file_path = input
        .and_then(|input| input.get("file_path"))
        .and_then(|value| value.as_str())
        .filter(|path| !path.is_empty())?;
    Some(get_display_path(file_path))
}

/// Maps to: CC `tools/FileEditTool/UI.tsx:74-87` `renderToolUseMessage`'s
/// linked-path representation: the path renders inside `<FilePathLink>`
/// (OSC-8 hyperlink), label = verbose ? full : display path; plan files
/// suppress the description entirely.
pub fn render_tool_use_path_link(
    input: &serde_json::Value,
    verbose: bool,
) -> Option<(String, String)> {
    let file_path = input
        .get("file_path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())?;
    if file_path.starts_with(&get_plans_directory().display().to_string()) {
        return None;
    }
    let label = if verbose {
        file_path.to_string()
    } else {
        get_display_path(file_path)
    };
    Some((file_path.to_string(), label))
}

/// Maps to CC `tools/FileEditTool/UI.tsx#renderToolUseMessage`.
pub fn render_tool_use_message(input: &serde_json::Value, verbose: bool) -> Option<String> {
    let file_path = input
        .get("file_path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())?;
    if file_path.starts_with(&get_plans_directory().display().to_string()) {
        return Some(String::new());
    }
    Some(if verbose {
        file_path.to_string()
    } else {
        get_display_path(file_path)
    })
}

pub fn render_tool_result_lines(
    path: Option<&str>,
    additions: usize,
    removals: usize,
    operation: &str,
    diff_lines: &[String],
    diff_hunks: &[StructuredDiffHunk],
    status: ToolResultStatus,
    fallback: &str,
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    // Maps to CC `renderToolUseErrorMessage`: intended validation failures are
    // compact in normal mode, while verbose mode falls through to full error.
    if status == ToolResultStatus::Error
        && !options.verbose
        && let Some(error) = crate::utils::messages::extract_tag(fallback, "tool_use_error")
    {
        if error.contains("File has not been read yet") {
            return vec![ToolRenderLine::new(
                "File must be read first",
                ToolRenderTone::Inactive,
            )];
        }
        if error.contains(crate::utils::file::FILE_NOT_FOUND_CWD_NOTE) {
            return vec![ToolRenderLine::new("File not found", ToolRenderTone::Error)];
        }
        return vec![ToolRenderLine::new(
            "Error editing file",
            ToolRenderTone::Error,
        )];
    }
    // The error tail delegates to FallbackToolUseErrorMessage exactly as CC
    // does (UI.tsx:209): "Error: " prefix, InvalidToolParameters mapping,
    // tag stripping, and the 10-line truncation with the ctrl+o hint.
    if status == ToolResultStatus::Error {
        return crate::components::fallback_tool_use_error_message::fallback_tool_use_error_lines(
            fallback,
            options.verbose,
        );
    }
    let target = path.unwrap_or("file");
    let summary = match status {
        ToolResultStatus::Success => diff_summary(additions, removals)
            .unwrap_or_else(|| status_text(fallback, &format!("Updated {target}"))),
        ToolResultStatus::Rejected => diff_summary(additions, removals)
            .map(|summary| format!("Rejected {operation} to {target} · {summary}"))
            .unwrap_or_else(|| format!("Rejected {operation} to {target}")),
        ToolResultStatus::Error => unreachable!("handled by the fallback tail above"),
        ToolResultStatus::Canceled => "Interrupted by user".to_string(),
    };

    let mut lines = vec![ToolRenderLine::new(summary, status_tone(status))];
    if matches!(
        status,
        ToolResultStatus::Success | ToolResultStatus::Rejected
    ) {
        lines.extend(render_structured_diff_lines(
            path, diff_lines, diff_hunks, options,
        ));
    }
    lines
}

fn diff_summary(additions: usize, removals: usize) -> Option<String> {
    let mut parts = Vec::new();
    if additions > 0 {
        parts.push(format!(
            "Added {additions} {}",
            plural(additions, "line", "lines")
        ));
    }
    if removals > 0 {
        // CC capitalizes when there are no additions:
        // `{numAdditions === 0 ? 'R' : 'r'}emoved` (FileEditToolUpdatedMessage.tsx:44).
        parts.push(format!(
            "{}emoved {removals} {}",
            if additions == 0 { "R" } else { "r" },
            plural(removals, "line", "lines")
        ));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn render_structured_diff_lines(
    path: Option<&str>,
    diff_lines: &[String],
    diff_hunks: &[StructuredDiffHunk],
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    let width = options.terminal_width.saturating_sub(12).max(1);
    let syntax = || structured_diff::SyntaxHighlightOptions {
        file_path: path.map(str::to_string),
        first_line: None,
        theme: options.syntax_theme,
        prefix_content: None,
    };
    if !diff_hunks.is_empty() {
        if options.syntax_highlighting && path.is_some() {
            structured_diff::render_hunks_with_syntax(diff_hunks, false, width, syntax())
        } else {
            structured_diff::render_hunks(diff_hunks, false, width)
        }
    } else if options.syntax_highlighting && path.is_some() {
        structured_diff::render_preformatted_lines_with_syntax(diff_lines, false, width, syntax())
    } else {
        structured_diff::render_preformatted_lines(diff_lines, false, width)
    }
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

fn status_text(fallback: &str, default: &str) -> String {
    let trimmed = fallback.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed
            .strip_prefix("Error: ")
            .unwrap_or(trimmed)
            .to_string()
    }
}

fn status_tone(status: ToolResultStatus) -> ToolRenderTone {
    // Maps to: CC `FileEditToolUpdatedMessage` — success uses default `<Text>`,
    // not `color="success"`.
    match status {
        ToolResultStatus::Success => ToolRenderTone::Normal,
        ToolResultStatus::Error => ToolRenderTone::Error,
        ToolResultStatus::Rejected => ToolRenderTone::Warning,
        ToolResultStatus::Canceled => ToolRenderTone::Inactive,
    }
}

// ─── Raw `toolUseResult` wire channel ────────────────────────────────────

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): all seven object keys are required,
/// `gitDiff` optional (`FileEditTool/types.ts:63-80`). The Rust transport
/// fields never ride the wire and reconstruct empty.
pub(crate) fn parse_output(
    value: &serde_json::Value,
) -> Option<crate::tools::file_edit_tool::types::FileEditOutput> {
    let map = value.as_object()?;
    Some(crate::tools::file_edit_tool::types::FileEditOutput {
        file_path: map.get("filePath")?.as_str()?.to_string(),
        old_string: map.get("oldString")?.as_str()?.to_string(),
        new_string: map.get("newString")?.as_str()?.to_string(),
        original_file: map.get("originalFile")?.as_str()?.to_string(),
        structured_patch: map
            .get("structuredPatch")?
            .as_array()?
            .iter()
            .map(crate::types::message::StructuredDiffHunk::from_official_json)
            .collect::<Option<Vec<_>>>()?,
        user_modified: map.get("userModified")?.as_bool()?,
        replace_all: map.get("replaceAll")?.as_bool()?,
        git_diff: match map.get("gitDiff") {
            None => None,
            Some(diff) => Some(crate::utils::git_diff::ToolUseDiff::from_official_json(
                diff,
            )?),
        },
        updated_file: String::new(),
        read_timestamp_ms: 0,
        dynamic_skill_dirs: Vec::new(),
    })
}

/// Serializes [`FileEditOutput`] to CC's exact `toolUseResult` wire shape —
/// the `call()` data construction order (`FileEditTool.ts:561-570`); `gitDiff`
/// spread-omitted when absent.
pub(crate) fn output_to_value(
    output: &crate::tools::file_edit_tool::types::FileEditOutput,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("filePath".to_string(), serde_json::json!(output.file_path));
    map.insert(
        "oldString".to_string(),
        serde_json::json!(output.old_string),
    );
    map.insert(
        "newString".to_string(),
        serde_json::json!(output.new_string),
    );
    map.insert(
        "originalFile".to_string(),
        serde_json::json!(output.original_file),
    );
    map.insert(
        "structuredPatch".to_string(),
        serde_json::Value::Array(
            output
                .structured_patch
                .iter()
                .map(crate::types::message::StructuredDiffHunk::to_official_json)
                .collect(),
        ),
    );
    map.insert(
        "userModified".to_string(),
        serde_json::json!(output.user_modified),
    );
    map.insert(
        "replaceAll".to_string(),
        serde_json::json!(output.replace_all),
    );
    if let Some(diff) = output.git_diff.as_ref() {
        map.insert("gitDiff".to_string(), diff.to_official_json());
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_error_copy_matches_official_edit_renderer() {
        let render = |content: &str| {
            render_tool_result_lines(
                None,
                0,
                0,
                "update",
                &[],
                &[],
                ToolResultStatus::Error,
                content,
                ToolRenderOptions::default(),
            )
        };
        assert_eq!(
            render("<tool_use_error>File has not been read yet. Read it first before writing to it.</tool_use_error>")[0].text,
            "File must be read first"
        );
        assert_eq!(
            render(&format!(
                "<tool_use_error>File does not exist. {} /repo.</tool_use_error>",
                crate::utils::file::FILE_NOT_FOUND_CWD_NOTE
            ))[0]
                .text,
            "File not found"
        );
        assert_eq!(
            render("<tool_use_error>String to replace not found in file.</tool_use_error>")[0].text,
            "Error editing file"
        );
    }

    /// Maps to: CC UI.tsx:209 — the error tail delegates to
    /// FallbackToolUseErrorMessage: "Error: " prefix and the 10-line
    /// truncation with the ctrl+o hint (verbose shows everything raw).
    #[test]
    fn error_tail_matches_official_fallback_renderer() {
        let render = |content: &str, verbose: bool| {
            render_tool_result_lines(
                None,
                0,
                0,
                "update",
                &[],
                &[],
                ToolResultStatus::Error,
                content,
                ToolRenderOptions {
                    verbose,
                    ..ToolRenderOptions::default()
                },
            )
        };
        // Verbose mode: raw error, the compact copy does not apply.
        let long = (1..=14)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = render(&long, false);
        assert_eq!(lines[0].text, "Error: line 1");
        assert_eq!(lines.len(), 11, "10 visible + truncation marker");
        assert!(lines[10].text.contains("+4 lines (ctrl+o to see all)"));
        // An untagged error in verbose mode keeps every raw line.
        let lines = render(&long, true);
        assert_eq!(lines.len(), 14);
        assert_eq!(lines[0].text, "Error: line 1");
        // InputValidationError maps to the official stable copy — assigned
        // directly, WITHOUT the "Error: " prefix (FallbackToolUseErrorMessage.tsx:39).
        assert_eq!(
            render("InputValidationError: bad", false)[0].text,
            "Invalid tool parameters"
        );
    }

    /// Maps to: CC FileEditToolUpdatedMessage.tsx:44 —
    /// `{numAdditions === 0 ? 'R' : 'r'}emoved`.
    #[test]
    fn diff_summary_capitalizes_removed_when_no_additions_like_official() {
        assert_eq!(diff_summary(0, 3).as_deref(), Some("Removed 3 lines"));
        assert_eq!(
            diff_summary(2, 3).as_deref(),
            Some("Added 2 lines, removed 3 lines")
        );
        assert_eq!(diff_summary(1, 0).as_deref(), Some("Added 1 line"));
        assert_eq!(diff_summary(0, 0), None);
    }

    #[test]
    fn rejection_diff_falls_back_to_input_diff_for_missing_files() {
        // The rejected preview is computed at render time from the tool
        // INPUT (`official_file_result_element`); this covers the ENOENT
        // branch — the diff comes from the tool inputs alone
        // (FileEditTool/UI.tsx:293-301).
        let missing = std::env::temp_dir().join(format!(
            "nyan-edit-rejected-missing-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let data = load_rejection_diff(&missing.display().to_string(), "old", "new", false);
        let added = data
            .patch
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.starts_with('+'))
            .count();
        let removed = data
            .patch
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.starts_with('-'))
            .count();
        assert_eq!((added, removed), (1, 1));
        assert!(data.first_line.is_none());
        assert!(data.file_content.is_none());
    }
}
