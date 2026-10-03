//! UI-only port of official `tools/FileWriteTool/UI.tsx` result rendering.

use crate::components::ctrl_o_to_expand::ctrl_o_to_expand_hint;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderTone,
};
use crate::components::structured_diff;
use crate::types::message::{StructuredDiffHunk, ToolResultStatus};

const MAX_LINES_TO_RENDER: usize = 10;

fn file_path(input: Option<&serde_json::Value>) -> Option<&str> {
    input?.get("file_path").and_then(serde_json::Value::as_str)
}

/// Maps to CC `FileWriteTool/UI.tsx#userFacingName`.
pub fn user_facing_name(input: Option<&serde_json::Value>) -> String {
    if file_path(input).is_some_and(|path| {
        path.starts_with(
            &crate::utils::plans::get_plans_directory()
                .display()
                .to_string(),
        )
    }) {
        "Updated plan".to_string()
    } else {
        "Write".to_string()
    }
}

/// Maps to CC `FileWriteTool/UI.tsx#getToolUseSummary`.
pub fn get_tool_use_summary(input: Option<&serde_json::Value>) -> Option<String> {
    file_path(input).map(crate::utils::file::get_display_path)
}

/// Maps to CC `tools/FileWriteTool/UI.tsx#RejectionDiffData` (:152-155).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RejectionDiffData {
    Create,
    Update {
        patch: Vec<StructuredDiffHunk>,
        old_content: String,
    },
    Error,
}

/// Maps to CC `tools/FileWriteTool/UI.tsx#loadRejectionDiff` (:230-262).
pub(crate) fn load_rejection_diff(file_path: &str, content: &str) -> RejectionDiffData {
    let load = || -> std::io::Result<RejectionDiffData> {
        let path = std::path::Path::new(file_path);
        let full_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            crate::bootstrap::state::get_original_cwd().join(path)
        };
        let Some(mut handle) = crate::utils::read_edit_context::open_for_scan(&full_path)? else {
            return Ok(RejectionDiffData::Create);
        };
        // Files past MAX_SCAN_BYTES fall back to the create view rather than
        // diffing a multi-GB buffer.
        let Some(old_content) = crate::utils::read_edit_context::read_capped(&mut handle)? else {
            return Ok(RejectionDiffData::Create);
        };
        let patch = crate::utils::diff::get_patch_for_display(
            &old_content,
            &[crate::utils::diff::DisplayEdit {
                old_string: &old_content,
                new_string: content,
                replace_all: false,
            }],
        );
        Ok(RejectionDiffData::Update { patch, old_content })
    };

    load().unwrap_or_else(|error| {
        // The user may have applied the write by hand while the diff was shown.
        crate::utils::debug::log_for_debugging(&format!(
            "Unable to load rejected Write diff: {error}"
        ));
        RejectionDiffData::Error
    })
}

/// Maps to: CC `tools/FileWriteTool/UI.tsx:282-335` `renderToolResultMessage`
/// — element-pipeline owner for the update arm (shares the Edit component,
/// `firstLine` comes from the NEW content, UI.tsx:320-333). The create arm
/// returns None and renders through the line pipeline
/// (`render_tool_result_lines`).
pub(crate) fn render_tool_result_message(
    output: &crate::tools::file_write_tool::WriteOutput,
    verbose: bool,
    style: Option<&str>,
) -> Option<iocraft::AnyElement<'static>> {
    use iocraft::prelude::*;
    let condensed = style == Some("condensed");
    match output.kind {
        crate::tools::file_write_tool::WriteOutputKind::Update => {
            let preview_hint =
                crate::tools::file_edit_tool::ui::plans_preview_hint(&output.file_path);
            let first_line = output
                .content
                .split_once('\n')
                .map_or(output.content.as_str(), |(first, _)| first)
                .to_string();
            Some(
                element! {
                    crate::components::file_edit_tool_updated_message::FileEditToolUpdatedMessage(
                        file_path: output.file_path.clone(),
                        structured_patch: output.structured_patch.clone(),
                        first_line: Some(first_line),
                        file_content: output.original_file.clone(),
                        verbose: verbose,
                        style: style.map(str::to_string),
                        preview_hint: preview_hint,
                    )
                }
                .into_any(),
            )
        }
        crate::tools::file_write_tool::WriteOutputKind::Create => {
            let is_plan_file = output.file_path.starts_with(
                &crate::utils::plans::get_plans_directory()
                    .display()
                    .to_string(),
            );
            // CC UI.tsx:292-309 — plan files invert condensed (regular mode
            // hints, condensed shows the full content via the created
            // message); non-plan condensed collapses to the bare one-line
            // summary with no MessageResponse wrapper.
            if !(is_plan_file && !verbose) && condensed && !verbose {
                let num_lines = count_lines(&output.content);
                let label = crate::utils::path::node_path_relative(
                    &crate::bootstrap::state::get_original_cwd(),
                    std::path::Path::new(&output.file_path),
                );
                return Some(
                    element! {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "Wrote ".to_string(), wrap: TextWrap::NoWrap)
                            Text(content: num_lines.to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                            Text(content: " lines to ".to_string(), wrap: TextWrap::NoWrap)
                            Text(content: label, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        }
                    }
                    .into_any(),
                );
            }
            // Plan-invert full content and the regular create preview both
            // render through the line pipeline (render_tool_result_lines).
            None
        }
    }
}

/// Maps to: CC `tools/FileWriteTool/UI.tsx:138-228`
/// `renderToolUseRejectedMessage` — existing files diff as an update,
/// missing/oversized files fall to the create preview, a read error renders
/// "(No changes)". The synchronous bounded read is the same L1 architectural
/// deviation recorded on the Edit-side twin (`file_edit_tool/ui.rs`).
pub(crate) fn render_tool_use_rejected_message(
    input: &serde_json::Value,
    verbose: bool,
    style: Option<&str>,
) -> Option<iocraft::AnyElement<'static>> {
    use crate::components::file_edit_tool_use_rejected_message::FileEditToolUseRejectedMessage;
    use crate::components::message_response::MessageResponse;
    use iocraft::prelude::*;
    let path = input.get("file_path").and_then(serde_json::Value::as_str)?;
    let content = input
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let first_line = content
        .split_once('\n')
        .map_or(content, |(first, _)| first)
        .to_string();
    let style_owned = style.map(str::to_string);
    match load_rejection_diff(path, content) {
        RejectionDiffData::Update { patch, old_content } => Some(
            element! {
                FileEditToolUseRejectedMessage(
                    file_path: path.to_string(),
                    operation: "update".to_string(),
                    patch: (!patch.is_empty()).then(|| patch.clone()),
                    first_line: Some(first_line.clone()),
                    file_content: Some(old_content.clone()),
                    verbose: verbose,
                    style: style_owned.clone(),
                )
            }
            .into_any(),
        ),
        RejectionDiffData::Create => Some(
            element! {
                FileEditToolUseRejectedMessage(
                    file_path: path.to_string(),
                    operation: "write".to_string(),
                    content: Some(content.to_string()),
                    first_line: Some(first_line.clone()),
                    verbose: verbose,
                    style: style_owned.clone(),
                )
            }
            .into_any(),
        ),
        RejectionDiffData::Error => Some(
            element! {
                MessageResponse {
                    Text(content: "(No changes)")
                }
            }
            .into_any(),
        ),
    }
}

/// Maps to: CC `tools/FileWriteTool/UI.tsx:120-136` `renderToolUseMessage`'s
/// linked-path representation (same FilePathLink shape as the Edit tool;
/// plan files suppress the description).
pub fn render_tool_use_path_link(
    input: &serde_json::Value,
    verbose: bool,
) -> Option<(String, String)> {
    let path = input
        .get("file_path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())?;
    if path.starts_with(
        &crate::utils::plans::get_plans_directory()
            .display()
            .to_string(),
    ) {
        return None;
    }
    let label = if verbose {
        path.to_string()
    } else {
        crate::utils::file::get_display_path(path)
    };
    Some((path.to_string(), label))
}

/// Maps to CC `FileWriteTool/UI.tsx#renderToolUseMessage`.
pub fn render_tool_use_message(input: Option<&serde_json::Value>, verbose: bool) -> Option<String> {
    let path = file_path(input)?;
    if path.starts_with(
        &crate::utils::plans::get_plans_directory()
            .display()
            .to_string(),
    ) {
        return Some(String::new());
    }
    Some(if verbose {
        path.to_string()
    } else {
        crate::utils::file::get_display_path(path)
    })
}

pub fn render_tool_result_lines(
    path: Option<&str>,
    line_count: Option<usize>,
    operation: &str,
    status: ToolResultStatus,
    fallback: &str,
    preview_content: Option<&str>,
    diff_lines: &[String],
    diff_hunks: &[StructuredDiffHunk],
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    let target = path.unwrap_or("file");
    let mut lines = Vec::new();

    if status != ToolResultStatus::Success {
        // Align the line-pipeline projection with the component leaves:
        // errors mirror renderToolUseErrorMessage (UI.tsx:264-280 — compact
        // "Error writing file" unless verbose, then the official fallback
        // renderer), instead of the former strip-"Error: "-and-show-all copy.
        if status == ToolResultStatus::Error {
            if !options.verbose
                && crate::utils::messages::extract_tag(fallback, "tool_use_error")
                    .is_some_and(|error| !error.is_empty())
            {
                return vec![ToolRenderLine::new(
                    "Error writing file",
                    ToolRenderTone::Error,
                )];
            }
            return crate::components::fallback_tool_use_error_message::fallback_tool_use_error_lines(
                fallback,
                options.verbose,
            );
        }
        if status == ToolResultStatus::Rejected {
            // CC FileEditToolUseRejectedMessage.tsx:38-43 — "User rejected
            // {operation} to {path}", path relative to cwd unless verbose.
            let display = if options.verbose {
                target.to_string()
            } else {
                crate::utils::path::node_path_relative(
                    &crate::bootstrap::state::get_original_cwd(),
                    std::path::Path::new(target),
                )
            };
            return vec![ToolRenderLine::new(
                format!("User rejected {operation} to {display}"),
                status_tone(status),
            )];
        }
        return vec![ToolRenderLine::new(
            status_text(status, fallback, "Error writing file"),
            status_tone(status),
        )];
    }

    if operation == "create" {
        let is_plan_file = path.is_some_and(|path| {
            path.starts_with(
                &crate::utils::plans::get_plans_directory()
                    .display()
                    .to_string(),
            )
        });
        // CC UI.tsx:294 gates on plain `verbose` (:285 destructures only
        // {style, verbose}; isTranscriptMode is passed but ignored).
        if is_plan_file && !options.verbose {
            return vec![ToolRenderLine::new(
                "/plan to preview",
                ToolRenderTone::Inactive,
            )];
        }
        let content_available = preview_content.is_some();
        let content = preview_content.unwrap_or("");
        let visible_count = line_count.unwrap_or_else(|| {
            if content_available {
                count_lines(content)
            } else {
                0
            }
        });
        let display_target = if options.verbose {
            target.to_string()
        } else {
            // CC UI.tsx:59 uses the bare `relative(getCwd(), filePath)` —
            // deliberately not getDisplayPath (outside-cwd paths become
            // `../..`, not `~/...`).
            crate::utils::path::node_path_relative(
                &crate::bootstrap::state::get_original_cwd(),
                std::path::Path::new(target),
            )
        };
        // CC UI.tsx:56-60: always-plural "lines", numLines and the path
        // both bold.
        lines.push(
            ToolRenderLine::new(
                format!("Wrote {visible_count} lines to {display_target}"),
                status_tone(status),
            )
            .with_segments(vec![
                crate::components::messages::user_tool_result_message::utils::ToolRenderSegment::new("Wrote "),
                crate::components::messages::user_tool_result_message::utils::ToolRenderSegment::new(
                    visible_count.to_string(),
                )
                .with_bold(true),
                crate::components::messages::user_tool_result_message::utils::ToolRenderSegment::new(" lines to "),
                crate::components::messages::user_tool_result_message::utils::ToolRenderSegment::new(display_target)
                    .with_bold(true),
            ]),
        );

        let preview = if !content_available {
            ""
        } else if content.is_empty() {
            "(No content)"
        } else {
            content
        };
        let preview_lines = preview.lines().collect::<Vec<_>>();
        // CC UI.tsx:64-71 truncates on plain `verbose` only.
        let take_count = if options.verbose {
            preview_lines.len()
        } else {
            preview_lines.len().min(MAX_LINES_TO_RENDER)
        };
        if take_count > 0 {
            let preview_text = preview_lines
                .iter()
                .take(take_count)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if options.syntax_highlighting {
                if let Some(path) = path {
                    lines.extend(structured_diff::ColorFile::new(preview_text, path).render(
                        options.syntax_theme,
                        options.terminal_width.saturating_sub(12).max(1),
                        false,
                    ));
                } else {
                    lines.push(ToolRenderLine::new(preview_text, ToolRenderTone::Inactive));
                }
            } else {
                lines.push(ToolRenderLine::new(preview_text, ToolRenderTone::Inactive));
            }
        }
        // CC UI.tsx:74-81: the "+N lines" marker keys on plain `verbose`.
        // Accepted line-pipeline loss (Write audit W9): CC's <CtrlOToExpand/>
        // suppresses itself inside SubAgent/InVirtualList contexts; the
        // string projection has no component context, so the hint always
        // renders here.
        if !options.verbose && visible_count > MAX_LINES_TO_RENDER {
            let hidden = visible_count - MAX_LINES_TO_RENDER;
            lines.push(ToolRenderLine::new(
                format!(
                    "… +{hidden} {} {}",
                    plural(hidden, "line", "lines"),
                    ctrl_o_to_expand_hint()
                ),
                ToolRenderTone::Inactive,
            ));
        }
        return lines;
    }

    let summary = diff_summary(diff_lines).unwrap_or_else(|| {
        let trimmed = fallback.trim();
        if !trimmed.is_empty() {
            trimmed
                .strip_prefix("Error: ")
                .unwrap_or(trimmed)
                .to_string()
        } else if let Some(lines_changed) = line_count {
            format!(
                "Updated {target} · {lines_changed} {}",
                plural(lines_changed, "line", "lines")
            )
        } else {
            format!("Updated {target}")
        }
    });
    lines.push(ToolRenderLine::new(summary, status_tone(status)));
    lines.extend(render_structured_diff_lines(
        path, diff_lines, diff_hunks, options,
    ));
    lines
}

fn diff_summary(diff_lines: &[String]) -> Option<String> {
    let additions = diff_lines
        .iter()
        .filter(|line| diff_line_marker(line) == Some('+'))
        .count();
    let removals = diff_lines
        .iter()
        .filter(|line| diff_line_marker(line) == Some('-'))
        .count();
    let mut parts = Vec::new();
    if additions > 0 {
        parts.push(format!(
            "Added {additions} {}",
            plural(additions, "line", "lines")
        ));
    }
    if removals > 0 {
        // CC FileEditToolUpdatedMessage.tsx:49: removals-only summaries
        // capitalize "Removed".
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

fn diff_line_marker(line: &str) -> Option<char> {
    let first = line.chars().next()?;
    if matches!(first, '+' | '-') {
        return Some(first);
    }
    let rest = line.trim_start();
    let rest = rest.trim_start_matches(|ch: char| ch.is_ascii_digit());
    rest.trim_start().chars().next()
}

/// Maps to CC `FileWriteTool/UI.tsx#countLines`.
pub(crate) fn count_lines(content: &str) -> usize {
    let parts = content.split('\n').count();
    if content.ends_with('\n') {
        parts.saturating_sub(1)
    } else {
        parts
    }
}

/// Maps to: CC `tools/FileWriteTool/UI.tsx:95-109` `isResultTruncated`:
/// only `create` truncates (update renders the full diff regardless of
/// verbose); early-exit scan for the (MAX+1)th line instead of splitting
/// the whole (possibly huge) content; a trailing EOL is a terminator, not
/// a new line.
pub(crate) fn is_result_truncated(output: &super::WriteOutput) -> bool {
    if output.kind != super::WriteOutputKind::Create {
        return false;
    }
    let content = output.content.as_str();
    let mut pos = 0usize;
    for _ in 0..MAX_LINES_TO_RENDER {
        match content[pos..].find('\n') {
            Some(offset) => pos += offset + 1,
            None => return false,
        }
    }
    pos < content.len()
}

fn status_text(status: ToolResultStatus, fallback: &str, error_default: &str) -> String {
    match status {
        ToolResultStatus::Success => unreachable!(),
        ToolResultStatus::Error => {
            let trimmed = fallback.trim();
            if trimmed.is_empty() {
                error_default.to_string()
            } else {
                trimmed
                    .strip_prefix("Error: ")
                    .unwrap_or(trimmed)
                    .to_string()
            }
        }
        ToolResultStatus::Rejected => "Tool use rejected".to_string(),
        ToolResultStatus::Canceled => "Interrupted by user".to_string(),
    }
}

fn status_tone(status: ToolResultStatus) -> ToolRenderTone {
    // Maps to: CC `FileWriteToolCreatedMessage` / edit-updated message — success
    // uses default `<Text>`, not `color="success"`.
    match status {
        ToolResultStatus::Success => ToolRenderTone::Normal,
        ToolResultStatus::Error => ToolRenderTone::Error,
        ToolResultStatus::Rejected => ToolRenderTone::Warning,
        ToolResultStatus::Canceled => ToolRenderTone::Inactive,
    }
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

// ─── Raw `toolUseResult` wire channel ────────────────────────────────────

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): `type` is a strict two-value enum,
/// `originalFile` is required-but-nullable, `gitDiff` optional
/// (`FileWriteTool.ts:68-89`). The Rust transport fields never ride the wire
/// and reconstruct empty.
pub(crate) fn parse_output(
    value: &serde_json::Value,
) -> Option<crate::tools::file_write_tool::WriteOutput> {
    let map = value.as_object()?;
    Some(crate::tools::file_write_tool::WriteOutput {
        kind: match map.get("type")?.as_str()? {
            "create" => crate::tools::file_write_tool::WriteOutputKind::Create,
            "update" => crate::tools::file_write_tool::WriteOutputKind::Update,
            _ => return None,
        },
        file_path: map.get("filePath")?.as_str()?.to_string(),
        content: map.get("content")?.as_str()?.to_string(),
        structured_patch: map
            .get("structuredPatch")?
            .as_array()?
            .iter()
            .map(crate::types::message::StructuredDiffHunk::from_official_json)
            .collect::<Option<Vec<_>>>()?,
        original_file: match map.get("originalFile")? {
            serde_json::Value::Null => None,
            serde_json::Value::String(original) => Some(original.clone()),
            _ => return None,
        },
        read_timestamp_ms: 0,
        git_diff: match map.get("gitDiff") {
            None => None,
            Some(diff) => Some(crate::utils::git_diff::ToolUseDiff::from_official_json(
                diff,
            )?),
        },
        dynamic_skill_dirs: Vec::new(),
    })
}

/// Serializes [`WriteOutput`] to CC's exact `toolUseResult` wire shape — the
/// shared `call()` data construction order for both branches
/// (`FileWriteTool.ts:372-378` update / `:395-401` create): type, filePath,
/// content, structuredPatch, originalFile (null for create), then `gitDiff`
/// spread-omitted when absent.
pub(crate) fn output_to_value(
    output: &crate::tools::file_write_tool::WriteOutput,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "type".to_string(),
        serde_json::json!(match output.kind {
            crate::tools::file_write_tool::WriteOutputKind::Create => "create",
            crate::tools::file_write_tool::WriteOutputKind::Update => "update",
        }),
    );
    map.insert("filePath".to_string(), serde_json::json!(output.file_path));
    map.insert("content".to_string(), serde_json::json!(output.content));
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
        "originalFile".to_string(),
        serde_json::json!(output.original_file),
    );
    if let Some(diff) = output.git_diff.as_ref() {
        map.insert("gitDiff".to_string(), diff.to_official_json());
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn plan_write_uses_updated_plan_name_and_hides_redundant_path() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let path = crate::utils::plans::get_plans_directory().join("plan.md");
        let input = serde_json::json!({"file_path": path, "content": "plan"});
        assert_eq!(user_facing_name(Some(&input)), "Updated plan");
        assert_eq!(
            render_tool_use_message(Some(&input), false).as_deref(),
            Some("")
        );
    }

    #[test]
    fn plan_create_result_uses_official_preview_hint_when_collapsed() {
        let path = crate::utils::plans::get_plans_directory().join("plan.md");
        let lines = render_tool_result_lines(
            Some(&path.display().to_string()),
            Some(2),
            "create",
            ToolResultStatus::Success,
            "",
            Some("one\ntwo"),
            &[],
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "/plan to preview");
    }

    #[test]
    fn create_result_collapses_absolute_path_relative_to_project_cwd() {
        struct CwdRestore(std::path::PathBuf);
        impl Drop for CwdRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(&self.0);
            }
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = CwdRestore(crate::bootstrap::state::get_original_cwd());
        let root = std::env::temp_dir().join(format!(
            "cometix-write-ui-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        crate::bootstrap::state::set_original_cwd(&root);
        let path = root.join("src/new.rs");
        let lines = render_tool_result_lines(
            Some(&path.display().to_string()),
            Some(1),
            "create",
            ToolResultStatus::Success,
            "",
            Some("line"),
            &[],
            &[],
            ToolRenderOptions::default(),
        );
        // CC UI.tsx:57 is always-plural "lines" — no singular branch.
        assert_eq!(lines[0].text, "Wrote 1 lines to src/new.rs");
        // CC UI.tsx:58-59: the count and the path render bold.
        let bold_texts = lines[0]
            .segments
            .iter()
            .filter(|segment| segment.bold)
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(bold_texts, vec!["1", "src/new.rs"]);
    }

    /// Maps to: CC `UI.tsx:59` — the compact path is the bare
    /// `relative(getCwd(), filePath)`, so an outside-cwd file renders as
    /// `../..`, not the `~/...` shape getDisplayPath would give.
    #[test]
    fn create_result_outside_cwd_uses_bare_relative_not_display_path() {
        struct CwdRestore(std::path::PathBuf);
        impl Drop for CwdRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(&self.0);
            }
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = CwdRestore(crate::bootstrap::state::get_original_cwd());
        let base = std::env::temp_dir().join(format!(
            "cometix-write-ui-rel-{}",
            uuid::Uuid::new_v4().simple()
        ));
        crate::bootstrap::state::set_original_cwd(&base.join("project"));
        let path = base.join("elsewhere/out.txt");
        let lines = render_tool_result_lines(
            Some(&path.display().to_string()),
            Some(1),
            "create",
            ToolResultStatus::Success,
            "",
            Some("line"),
            &[],
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "Wrote 1 lines to ../elsewhere/out.txt");
    }

    /// Maps to: CC `UI.tsx:285` — renderToolResultMessage destructures only
    /// {style, verbose}, so is_transcript_mode alone changes nothing here.
    /// This exact combination (transcript on, verbose off) is unreachable in
    /// the main pipeline — CC's transcript screen mounts Messages with
    /// verbose={true} (REPL.tsx:5823) and repl.rs:1433 mirrors it — the test
    /// locks the function's parameter contract, not a reachable screen state.
    #[test]
    fn create_result_transcript_mode_still_truncates_like_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let content = (0..12)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let transcript = ToolRenderOptions {
            is_transcript_mode: true,
            syntax_highlighting: false,
            ..ToolRenderOptions::default()
        };
        let lines = render_tool_result_lines(
            Some("/tmp/f.txt"),
            Some(12),
            "create",
            ToolResultStatus::Success,
            "",
            Some(&content),
            &[],
            &[],
            transcript,
        );
        let joined = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("l0"), "lines=\n{joined}");
        assert!(!joined.contains("l11"), "lines=\n{joined}");
        assert!(joined.contains("+2 lines"), "lines=\n{joined}");

        // Plan hint likewise keys on plain verbose.
        let plan_path = crate::utils::plans::get_plans_directory().join("plan.md");
        let plan_lines = render_tool_result_lines(
            Some(&plan_path.display().to_string()),
            Some(2),
            "create",
            ToolResultStatus::Success,
            "",
            Some("one\ntwo"),
            &[],
            &[],
            transcript,
        );
        assert_eq!(plan_lines.len(), 1);
        assert_eq!(plan_lines[0].text, "/plan to preview");
    }

    #[test]
    fn count_lines_treats_trailing_newline_as_terminator() {
        assert_eq!(count_lines(""), 1);
        assert_eq!(count_lines("one\n"), 1);
        assert_eq!(count_lines("one\ntwo"), 2);
    }
}
