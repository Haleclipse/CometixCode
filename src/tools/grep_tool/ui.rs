//! UI-only port of CC 2.1.88 `tools/GrepTool/UI.tsx`.
//! Glob reuses the same `SearchResultSummary` projection in CC.

use super::Output;
use crate::components::ctrl_o_to_expand::ctrl_o_to_expand_hint;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderSegment, ToolRenderTone,
};
use crate::types::message::{SearchResultMode, ToolResultStatus};
use crate::utils::truncate::truncate_to_width;

/// Maps to CC `GrepTool/UI.tsx:84-98` `renderToolUseMessage`.
pub fn render_tool_use_message(input: &serde_json::Value, verbose: bool) -> Option<String> {
    let pattern = input
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .filter(|pattern| !pattern.is_empty())?;
    let mut parts = vec![format!("pattern: \"{pattern}\"")];
    if let Some(path) = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let display = if verbose {
            path.to_string()
        } else {
            crate::utils::file::get_display_path(path)
        };
        parts.push(format!("path: \"{display}\""));
    }
    Some(parts.join(", "))
}

/// Maps to CC `GrepTool/UI.tsx:174-205` `getToolUseSummary`.
pub fn get_tool_use_summary(input: Option<&serde_json::Value>) -> Option<String> {
    let pattern = input?
        .get("pattern")?
        .as_str()
        .filter(|pattern| !pattern.is_empty())?;
    Some(truncate_to_width(
        pattern,
        crate::constants::tool_limits::TOOL_SUMMARY_MAX_LENGTH,
    ))
}

/// Maps to: CC `GrepTool/UI.tsx:126-172` `renderToolResultMessage` as invoked
/// by `UserToolSuccessMessage.tsx:80-96` — parse the raw `toolUseResult` with
/// the tool's own output schema, render the SearchResultSummary from the
/// parsed value, and render nothing when it does not parse.
///
/// This is the by-tool-name entry the dispatch calls — the only Grep
/// render path now that the Grep display variants are gone.
pub(crate) fn render_tool_result_message(
    raw_output: Option<&serde_json::Value>,
    status: ToolResultStatus,
    fallback: &str,
    options: &ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    // CC bails on a missing `toolUseResult` before touching the tool
    // (`UserToolSuccessMessage.tsx:72`).
    let Some(raw_output) = raw_output else {
        return Vec::new();
    };
    if status != ToolResultStatus::Success {
        return vec![ToolRenderLine::new(
            search_error_text(fallback, status, options.verbose),
            status_tone(status),
        )];
    }
    // CC: `safeParse` failure returns null, i.e. the row renders nothing
    // (`UserToolSuccessMessage.tsx:81`).
    let Some(output) = parse_output(raw_output) else {
        return Vec::new();
    };
    render_output(&output, options)
}

/// The body of CC `renderToolResultMessage` after `safeParse` — it receives
/// the parsed `Output` object. Split from the raw entry above because Glob
/// reuses exactly this function (`GlobTool/UI.tsx:56
/// `renderToolResultMessage = GrepTool.renderToolResultMessage`) with its own
/// parsed output.
pub(crate) fn render_output(output: &Output, options: &ToolRenderOptions) -> Vec<ToolRenderLine> {
    let zero = serde_json::Number::from(0);
    let mode = output.effective_mode();
    let count = match mode {
        SearchResultMode::Content => output.num_lines.as_ref().unwrap_or(&zero),
        SearchResultMode::Count => output.num_matches.as_ref().unwrap_or(&zero),
        SearchResultMode::FilesWithMatches => &output.num_files,
    };
    let detail = match mode {
        SearchResultMode::FilesWithMatches => Some(output.filenames.join("\n")),
        _ => output.content.clone(),
    };
    let secondary = (mode == SearchResultMode::Count).then_some(&output.num_files);
    render_success_summary(
        mode,
        &super::javascript_number_text(count),
        count.as_f64().unwrap_or(0.0),
        secondary
            .map(|number| {
                (
                    super::javascript_number_text(number),
                    number.as_f64().unwrap_or(0.0),
                )
            })
            .as_ref()
            .map(|(text, value)| (text.as_str(), *value)),
        detail,
        options.verbose,
    )
}

/// Maps to CC `GrepTool/UI.tsx:15-82` `SearchResultSummary`.
fn render_success_summary(
    mode: SearchResultMode,
    count_text: &str,
    count_value: f64,
    secondary: Option<(&str, f64)>,
    detail: Option<String>,
    verbose: bool,
) -> Vec<ToolRenderLine> {
    let (count_label, secondary_label) = match mode {
        SearchResultMode::Content => ("lines", None),
        SearchResultMode::Count => ("matches", Some("files")),
        SearchResultMode::FilesWithMatches => ("files", None),
    };
    // Deliberately mirrors `countLabel.slice(0, -1)`: one match is "matche".
    let primary_label = displayed_count_label(count_value, count_label);
    let mut summary = format!("Found {count_text} {primary_label}");
    let mut segments = vec![
        ToolRenderSegment::new("Found "),
        ToolRenderSegment::new(format!("{count_text} ")).with_bold(true),
        ToolRenderSegment::new(primary_label),
    ];
    if let (Some((secondary_text, secondary_value)), Some(label)) = (secondary, secondary_label) {
        let displayed = displayed_count_label(secondary_value, label);
        summary.push_str(&format!(" across {secondary_text} {displayed}"));
        segments.extend([
            ToolRenderSegment::new(" across "),
            ToolRenderSegment::new(format!("{secondary_text} ")).with_bold(true),
            ToolRenderSegment::new(displayed),
        ]);
    }

    if !verbose {
        if count_value > 0.0 {
            let hint = ctrl_o_to_expand_hint();
            let text = format!("{summary} {hint}");
            segments.push(ToolRenderSegment::new(" "));
            segments.push(ToolRenderSegment::new(hint).with_dim(true));
            return vec![ToolRenderLine::new(text, ToolRenderTone::Normal).with_segments(segments)];
        }
        return vec![ToolRenderLine::new(summary, ToolRenderTone::Normal).with_segments(segments)];
    }

    let mut lines =
        vec![ToolRenderLine::new(summary, ToolRenderTone::Normal).with_segments(segments)];
    if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        lines.push(ToolRenderLine::new(detail, ToolRenderTone::Normal));
    }
    lines
}

fn displayed_count_label(value: f64, plural: &'static str) -> &'static str {
    if value == 0.0 || value > 1.0 {
        plural
    } else {
        plural.strip_suffix('s').unwrap_or(plural)
    }
}

/// Maps to CC `GrepTool/UI.tsx:100-124` `renderToolUseErrorMessage`.
pub fn search_tool_use_error_message(result: &str, verbose: bool) -> Option<&'static str> {
    if verbose {
        return None;
    }
    let extracted = crate::utils::messages::extract_tag(result, "tool_use_error")?;
    if extracted.contains(crate::utils::file::FILE_NOT_FOUND_CWD_NOTE) {
        Some("File not found")
    } else {
        Some("Error searching files")
    }
}

fn search_error_text(fallback: &str, status: ToolResultStatus, verbose: bool) -> String {
    match status {
        ToolResultStatus::Success => unreachable!(),
        ToolResultStatus::Rejected => "Tool use rejected".to_string(),
        ToolResultStatus::Canceled => "Interrupted by user".to_string(),
        ToolResultStatus::Error => {
            if let Some(short) = search_tool_use_error_message(fallback, verbose) {
                return short.to_string();
            }
            let extracted = crate::utils::messages::extract_tag(fallback, "tool_use_error")
                .unwrap_or_else(|| fallback.to_string());
            let trimmed = extracted
                .replace("<error>", "")
                .replace("</error>", "")
                .trim()
                .to_string();
            if !verbose && trimmed.contains("InputValidationError: ") {
                "Invalid tool parameters".to_string()
            } else if trimmed.starts_with("Error: ") || trimmed.starts_with("Cancelled: ") {
                trimmed
            } else if trimmed.is_empty() {
                "Error: Tool execution failed".to_string()
            } else {
                format!("Error: {trimmed}")
            }
        }
    }
}

fn status_tone(status: ToolResultStatus) -> ToolRenderTone {
    match status {
        ToolResultStatus::Success => ToolRenderTone::Normal,
        ToolResultStatus::Error => ToolRenderTone::Error,
        ToolResultStatus::Rejected => ToolRenderTone::Warning,
        ToolResultStatus::Canceled => ToolRenderTone::Inactive,
    }
}

// ─── Typed Grep output (CC `toolUseResult` recovery) ─────────────────────

fn parse_optional_number(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<Option<serde_json::Number>> {
    match map.get(key) {
        None => Some(None),
        Some(serde_json::Value::Number(number)) => Some(Some(number.clone())),
        Some(_) => None,
    }
}

fn parse_optional_string(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<Option<String>> {
    match map.get(key) {
        None => Some(None),
        Some(serde_json::Value::String(value)) => Some(Some(value.clone())),
        Some(_) => None,
    }
}

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): strictly validates the
/// `GrepTool.outputSchema` required/type contract; unknown object keys are
/// stripped exactly as Zod's default object behavior.
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<Output> {
    let map = value.as_object()?;
    let mode = match map.get("mode") {
        None => None,
        Some(serde_json::Value::String(mode)) => Some(match mode.as_str() {
            "content" => SearchResultMode::Content,
            "count" => SearchResultMode::Count,
            "files_with_matches" => SearchResultMode::FilesWithMatches,
            _ => return None,
        }),
        Some(_) => return None,
    };
    let num_files = map.get("numFiles")?.as_number()?.clone();
    let filenames = map
        .get("filenames")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    Some(Output {
        mode,
        num_files,
        filenames,
        content: parse_optional_string(map, "content")?,
        num_lines: parse_optional_number(map, "numLines")?,
        num_matches: parse_optional_number(map, "numMatches")?,
        applied_limit: parse_optional_number(map, "appliedLimit")?,
        applied_offset: parse_optional_number(map, "appliedOffset")?,
    })
}

/// Serializes [`Output`] back to CC's exact `toolUseResult` wire shape —
/// optional fields absent, never null.
pub(crate) fn output_to_value(output: &Output) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(mode) = output.mode {
        map.insert(
            "mode".to_string(),
            serde_json::Value::String(
                match mode {
                    SearchResultMode::Content => "content",
                    SearchResultMode::Count => "count",
                    SearchResultMode::FilesWithMatches => "files_with_matches",
                }
                .to_string(),
            ),
        );
    }
    map.insert(
        "numFiles".to_string(),
        serde_json::Value::Number(output.num_files.clone()),
    );
    map.insert(
        "filenames".to_string(),
        serde_json::Value::Array(
            output
                .filenames
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    for (key, value) in [
        (
            "content",
            output
                .content
                .as_ref()
                .map(|value| serde_json::Value::String(value.clone())),
        ),
        (
            "numLines",
            output
                .num_lines
                .as_ref()
                .map(|value| serde_json::Value::Number(value.clone())),
        ),
        (
            "numMatches",
            output
                .num_matches
                .as_ref()
                .map(|value| serde_json::Value::Number(value.clone())),
        ),
        (
            "appliedLimit",
            output
                .applied_limit
                .as_ref()
                .map(|value| serde_json::Value::Number(value.clone())),
        ),
        (
            "appliedOffset",
            output
                .applied_offset
                .as_ref()
                .map(|value| serde_json::Value::Number(value.clone())),
        ),
    ] {
        if let Some(value) = value {
            map.insert(key.to_string(), value);
        }
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn use_message_and_summary_match_official_copy_and_limits() {
        let input = serde_json::json!({
            "pattern": "x".repeat(80),
            "path": "/tmp/project/src"
        });
        let use_message = render_tool_use_message(&input, true).expect("message");
        assert!(use_message.contains(&"x".repeat(80)));
        let summary = get_tool_use_summary(Some(&input)).expect("summary");
        assert_eq!(summary, format!("{}…", "x".repeat(49)));
        assert_eq!(get_tool_use_summary(None), None);
    }

    #[test]
    fn search_summary_preserves_official_match_singular_slice_bug() {
        let raw = serde_json::json!({
            "mode": "count",
            "numFiles": 1,
            "filenames": [],
            "content": "a.rs:1",
            "numMatches": 1
        });
        let lines = render_tool_result_message(
            Some(&raw),
            ToolResultStatus::Success,
            "",
            &ToolRenderOptions::default(),
        );
        assert!(lines[0].text.starts_with("Found 1 matche across 1 file"));
    }

    #[test]
    fn collapsed_and_verbose_results_match_search_result_summary() {
        let raw = serde_json::json!({
            "mode": "files_with_matches",
            "numFiles": 3,
            "filenames": ["a.rs", "b.rs", "c.rs"]
        });
        let short = render_tool_result_message(
            Some(&raw),
            ToolResultStatus::Success,
            "",
            &ToolRenderOptions::default(),
        );
        assert_eq!(short.len(), 1);
        assert!(short[0].text.starts_with("Found 3 files"));
        assert!(short[0].text.contains("ctrl+o to expand"));
        assert!(!short[0].text.contains("a.rs"));

        let empty = render_tool_result_message(
            Some(&serde_json::json!({
                "mode": "files_with_matches",
                "numFiles": 0,
                "filenames": []
            })),
            ToolResultStatus::Success,
            "",
            &ToolRenderOptions::default(),
        );
        assert_eq!(empty[0].text, "Found 0 files");
        assert!(!empty[0].text.contains("ctrl+o to expand"));

        let expanded = render_tool_result_message(
            Some(&raw),
            ToolResultStatus::Success,
            "",
            &ToolRenderOptions {
                verbose: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(expanded[0].text, "Found 3 files");
        assert_eq!(expanded[1].text, "a.rs\nb.rs\nc.rs");
    }

    #[test]
    fn error_renderer_matches_official_special_and_fallback_paths() {
        assert_eq!(
            search_tool_use_error_message(
                "<tool_use_error>Note: your current working directory is /tmp</tool_use_error>",
                false,
            ),
            Some("File not found")
        );
        assert_eq!(
            search_tool_use_error_message("<tool_use_error>boom</tool_use_error>", false),
            Some("Error searching files")
        );
        assert_eq!(
            search_error_text("plain error", ToolResultStatus::Error, false),
            "Error: plain error"
        );
    }

    #[test]
    fn unrestricted_output_numbers_render_with_javascript_number_and_label_semantics() {
        let raw = serde_json::json!({
            "mode": "count",
            "numFiles": "1e21".parse::<serde_json::Number>().unwrap(),
            "filenames": [],
            "numMatches": 0.5
        });
        let lines = render_tool_result_message(
            Some(&raw),
            ToolResultStatus::Success,
            "",
            &ToolRenderOptions::default(),
        );
        assert!(
            lines[0]
                .text
                .starts_with("Found 0.5 matche across 1e+21 files")
        );
        assert!(lines[0].text.contains("ctrl+o to expand"));
    }

    #[test]
    fn strict_output_parser_rejects_wrong_required_or_optional_types() {
        let valid = serde_json::json!({
            "mode": "content",
            "numFiles": -0.5,
            "filenames": [],
            "content": "a.rs:1:x",
            "numLines": 1.5,
            "appliedLimit": -2,
            "extra": true
        });
        let parsed = parse_output(&valid).expect("valid Zod output");
        assert_eq!(output_to_value(&parsed)["numLines"], serde_json::json!(1.5));
        assert!(output_to_value(&parsed).get("extra").is_none());
        assert!(parse_output(&serde_json::json!({"numFiles": 1})).is_none());
        assert!(
            parse_output(&serde_json::json!({
                "numFiles": 1,
                "filenames": [],
                "content": null
            }))
            .is_none()
        );
    }

    /// CC returns null when `toolUseResult` is absent or fails `safeParse`
    /// (`UserToolSuccessMessage.tsx:72,81`), i.e. the row renders nothing.
    #[test]
    fn raw_entry_renders_nothing_when_output_is_absent_or_unparseable() {
        assert!(
            render_tool_result_message(
                None,
                ToolResultStatus::Success,
                "fallback",
                &ToolRenderOptions::default(),
            )
            .is_empty()
        );
        assert!(
            render_tool_result_message(
                Some(&serde_json::json!({"numFiles": 1})),
                ToolResultStatus::Success,
                "fallback",
                &ToolRenderOptions::default(),
            )
            .is_empty()
        );
    }

    #[test]
    fn display_path_test_restores_bootstrap_state_with_raii() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        struct OriginalCwdGuard(std::path::PathBuf);
        impl Drop for OriginalCwdGuard {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(self.0.clone());
            }
        }
        let _guard = OriginalCwdGuard(crate::bootstrap::state::get_original_cwd());
        let cwd = std::env::temp_dir().join("cometix-grep-tool-use-display");
        let _ = std::fs::create_dir_all(&cwd);
        crate::bootstrap::state::set_original_cwd(&cwd);
        let input = serde_json::json!({
            "pattern": "needle",
            "path": cwd.join("src").to_string_lossy(),
        });
        assert_eq!(
            render_tool_use_message(&input, false).as_deref(),
            Some("pattern: \"needle\", path: \"src\"")
        );
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
