//! UI-only port of official `tools/BashTool/UI.tsx` and
//! `tools/BashTool/BashToolResultMessage.tsx`.

/// Maps to: CC `tools/BashTool/BashTool.tsx:720-730` `getToolUseSummary` —
/// `null` without a command, else the `description` when present, else the
/// truncated command. Both guards are JS-truthy.
pub fn get_tool_use_summary(input: Option<&serde_json::Value>) -> Option<String> {
    let input = input?;
    let command = input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .filter(|command| !command.is_empty())?;
    if let Some(description) = input
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

use crate::components::ctrl_o_to_expand::ctrl_o_to_expand_hint;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderTone,
};
use crate::tools::bash_tool::sed_edit_parser::parse_sed_edit_command;
use crate::types::message::ToolResultStatus;
use crate::utils::file::get_display_path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_COMMAND_DISPLAY_LINES: usize = 2;
const MAX_COMMAND_DISPLAY_CHARS: usize = 160;
/// Maps to CC Bash `OutputLine` success-path fold limit.
const MAX_OUTPUT_LINES: usize = 3;
const OUTPUT_WRAP_PADDING: usize = 10;
const MAX_JSON_FORMAT_LENGTH: usize = 10_000;
const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub fn render_tool_use_message(command: &str, options: ToolRenderOptions) -> String {
    if let Some(sed_info) = parse_sed_edit_command(command) {
        return if options.show_full() {
            sed_info.file_path
        } else {
            get_display_path(&sed_info.file_path)
        };
    }

    if options.show_full() {
        return command.to_string();
    }

    let lines = command.lines().collect::<Vec<_>>();
    let needs_line_truncation = lines.len() > MAX_COMMAND_DISPLAY_LINES;
    let needs_char_truncation = command.chars().count() > MAX_COMMAND_DISPLAY_CHARS;
    if !needs_line_truncation && !needs_char_truncation {
        return command.to_string();
    }

    let mut truncated = if needs_line_truncation {
        lines
            .into_iter()
            .take(MAX_COMMAND_DISPLAY_LINES)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        command.to_string()
    };
    if truncated.chars().count() > MAX_COMMAND_DISPLAY_CHARS {
        truncated = truncated.chars().take(MAX_COMMAND_DISPLAY_CHARS).collect();
    }
    format!("{}…", truncated.trim())
}

pub(crate) fn push_output_lines(
    lines: &mut Vec<ToolRenderLine>,
    output: &str,
    tone: ToolRenderTone,
    options: ToolRenderOptions,
) {
    let output = output.trim_end();
    if output.is_empty() {
        return;
    }
    let output = strip_underline_ansi(&try_json_format_content(output));

    if options.show_full() {
        lines.push(ToolRenderLine::new(output, tone));
        return;
    }

    let truncated = render_truncated_content(&output, options.terminal_width);
    if !truncated.above_the_fold.is_empty() {
        lines.push(ToolRenderLine::new(truncated.above_the_fold, tone));
    }
    if truncated.remaining_lines > 0 {
        lines.push(ToolRenderLine::new(
            format!(
                "… +{} lines {}",
                truncated.remaining_lines,
                ctrl_o_to_expand_hint()
            ),
            ToolRenderTone::Inactive,
        ));
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TruncatedContent {
    pub(crate) above_the_fold: String,
    pub(crate) remaining_lines: usize,
}

pub(crate) fn try_json_format_content(content: &str) -> String {
    if content.len() > MAX_JSON_FORMAT_LENGTH {
        return content.to_string();
    }
    content
        .split('\n')
        .map(try_format_json_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn try_format_json_line(line: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    if json_value_has_js_unsafe_number(&parsed) {
        return line.to_string();
    }
    let Ok(compact) = serde_json::to_string(&parsed) else {
        return line.to_string();
    };
    if normalize_json_for_round_trip(line) != normalize_json_for_round_trip(&compact) {
        return line.to_string();
    }
    serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| line.to_string())
}

fn normalize_json_for_round_trip(value: &str) -> String {
    value
        .replace("\\/", "/")
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

fn json_value_has_js_unsafe_number(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => {
            if let Some(unsigned) = number.as_u64() {
                return unsigned > JS_MAX_SAFE_INTEGER;
            }
            if let Some(signed) = number.as_i64() {
                return signed.unsigned_abs() > JS_MAX_SAFE_INTEGER;
            }
            number
                .as_f64()
                .is_some_and(|float| float.abs() > JS_MAX_SAFE_INTEGER as f64)
        }
        serde_json::Value::Array(items) => items.iter().any(json_value_has_js_unsafe_number),
        serde_json::Value::Object(map) => map.values().any(json_value_has_js_unsafe_number),
        _ => false,
    }
}

pub(crate) fn render_truncated_content(content: &str, terminal_width: usize) -> TruncatedContent {
    let trimmed = content.trim_end();
    if trimmed.is_empty() {
        return TruncatedContent::default();
    }

    let wrap_width = terminal_width.saturating_sub(OUTPUT_WRAP_PADDING).max(10);
    let max_chars = MAX_OUTPUT_LINES * wrap_width * 4;
    let char_count = trimmed.chars().count();
    let pre_truncated = char_count > max_chars;
    let content_for_wrapping = if pre_truncated {
        trimmed.chars().take(max_chars).collect::<String>()
    } else {
        trimmed.to_string()
    };

    let wrapped_lines = wrap_text(&content_for_wrapping, wrap_width);
    let remaining_lines = wrapped_lines.len().saturating_sub(MAX_OUTPUT_LINES);
    let estimated_remaining = if pre_truncated {
        remaining_lines.max(
            char_count
                .div_ceil(wrap_width)
                .saturating_sub(MAX_OUTPUT_LINES),
        )
    } else {
        remaining_lines
    };

    if remaining_lines == 1 {
        return TruncatedContent {
            above_the_fold: wrapped_lines
                .iter()
                .take(MAX_OUTPUT_LINES + 1)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
                .trim_end()
                .to_string(),
            remaining_lines: 0,
        };
    }

    TruncatedContent {
        above_the_fold: wrapped_lines
            .iter()
            .take(MAX_OUTPUT_LINES)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string(),
        remaining_lines: estimated_remaining,
    }
}

fn wrap_text(text: &str, wrap_width: usize) -> Vec<String> {
    let mut wrapped = Vec::new();
    for line in text.split('\n') {
        if display_width_ansi(line) <= wrap_width {
            wrapped.push(line.trim_end().to_string());
        } else {
            wrap_long_line(line, wrap_width, &mut wrapped);
        }
    }
    wrapped
}

fn wrap_long_line(line: &str, wrap_width: usize, wrapped: &mut Vec<String>) {
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut active_sgr = String::new();
    let mut idx = 0usize;

    while idx < line.len() {
        if let Some(end) = ansi_escape_end(line, idx) {
            let sequence = &line[idx..end];
            current.push_str(sequence);
            update_active_sgr_from_text(&mut active_sgr, sequence);
            idx = end;
            continue;
        }

        let Some(ch) = line[idx..].chars().next() else {
            break;
        };
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && ch_width > 0 && current_width + ch_width > wrap_width {
            push_wrapped_output_line(wrapped, &mut current, &mut current_width, &active_sgr);
        }
        current.push(ch);
        current_width += ch_width;
        idx += ch.len_utf8();
    }

    if !current.is_empty() {
        let mut line = current.trim_end().to_string();
        if !active_sgr.is_empty() {
            line.push_str("\x1b[0m");
        }
        wrapped.push(line);
    }
}

fn push_wrapped_output_line(
    wrapped: &mut Vec<String>,
    current: &mut String,
    current_width: &mut usize,
    active_sgr: &str,
) {
    let mut line = std::mem::take(current).trim_end().to_string();
    if !active_sgr.is_empty() {
        line.push_str("\x1b[0m");
    }
    wrapped.push(line);
    *current_width = 0;
    if !active_sgr.is_empty() {
        current.push_str(active_sgr);
    }
}

fn update_active_sgr_from_text(active_sgr: &mut String, text: &str) {
    let mut idx = 0usize;
    while idx < text.len() {
        let Some((end, params)) = csi_sgr_params(text, idx) else {
            idx += text[idx..].chars().next().map(char::len_utf8).unwrap_or(1);
            continue;
        };
        let sequence = &text[idx..end];
        if sgr_params_reset_style(params) {
            active_sgr.clear();
        } else if sgr_params_start_style(params) {
            active_sgr.push_str(sequence);
        }
        idx = end;
    }
}

fn csi_sgr_params(input: &str, start: usize) -> Option<(usize, &str)> {
    if input.as_bytes().get(start) != Some(&0x1b) || !input[start..].starts_with("\x1b[") {
        return None;
    }
    let rest = &input[start + 2..];
    let final_rel = rest.find(|ch: char| ('@'..='~').contains(&ch))?;
    let final_idx = start + 2 + final_rel;
    let final_char = input[final_idx..].chars().next()?;
    if final_char != 'm' {
        return None;
    }
    Some((
        final_idx + final_char.len_utf8(),
        &input[start + 2..final_idx],
    ))
}

fn sgr_params_reset_style(params: &str) -> bool {
    params.is_empty()
        || params
            .split(';')
            .filter(|part| !part.is_empty())
            .any(|part| matches!(part, "0" | "22" | "23" | "24" | "39" | "49"))
}

fn sgr_params_start_style(params: &str) -> bool {
    params
        .split(';')
        .filter_map(|part| part.parse::<u16>().ok())
        .any(|value| matches!(value, 1 | 2 | 3 | 4 | 30..=37 | 38 | 40..=47 | 48 | 90..=97 | 100..=107))
}

fn display_width_ansi(input: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi_for_width(input).as_str())
}

pub(crate) fn strip_underline_ansi(input: &str) -> String {
    let mut out = String::new();
    let mut idx = 0usize;
    while idx < input.len() {
        if let Some((end, is_underline)) = csi_sgr_escape(input, idx) {
            if !is_underline {
                out.push_str(&input[idx..end]);
            }
            idx = end;
            continue;
        }
        let Some(ch) = input[idx..].chars().next() else {
            break;
        };
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn csi_sgr_escape(input: &str, start: usize) -> Option<(usize, bool)> {
    if input.as_bytes().get(start) != Some(&0x1b) || !input[start..].starts_with("\x1b[") {
        return None;
    }
    let rest = &input[start + 2..];
    let final_rel = rest.find(|ch: char| ('@'..='~').contains(&ch))?;
    let final_idx = start + 2 + final_rel;
    let final_char = input[final_idx..].chars().next()?;
    if final_char != 'm' {
        return Some((final_idx + final_char.len_utf8(), false));
    }
    let params = &input[start + 2..final_idx];
    let has_underline = params
        .split(';')
        .filter(|part| !part.is_empty())
        .any(|part| part == "4");
    Some((final_idx + final_char.len_utf8(), has_underline))
}

fn strip_ansi_for_width(input: &str) -> String {
    let mut out = String::new();
    let mut idx = 0;
    while idx < input.len() {
        if let Some(end) = ansi_escape_end(input, idx) {
            idx = end;
            continue;
        }
        let Some(ch) = input[idx..].chars().next() else {
            break;
        };
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn ansi_escape_end(input: &str, start: usize) -> Option<usize> {
    if input.as_bytes().get(start) != Some(&0x1b) {
        return None;
    }

    let rest = &input[start..];
    if rest.starts_with("\x1b[") {
        let final_rel = rest[2..].find(|ch: char| ('@'..='~').contains(&ch))?;
        let final_idx = start + 2 + final_rel;
        let final_char = input[final_idx..].chars().next()?;
        return Some(final_idx + final_char.len_utf8());
    }

    if rest.starts_with("\x1b]") {
        let body_start = start + 2;
        let bel_end = input[body_start..]
            .find('\x07')
            .map(|rel| body_start + rel + 1);
        let st_end = input[body_start..]
            .find("\x1b\\")
            .map(|rel| body_start + rel + 2);
        return match (bel_end, st_end) {
            (Some(bel), Some(st)) => Some(bel.min(st)),
            (Some(bel), None) => Some(bel),
            (None, Some(st)) => Some(st),
            (None, None) => None,
        };
    }

    let mut end = start + 1;
    if let Some(next) = input[end..].chars().next() {
        end += next.len_utf8();
    }
    Some(end)
}

// ─── Recorded tool-result display parsing ────────────────────────────────
// Maps to: CC `tools/BashTool/UI.tsx` `renderToolResultMessage` (:174).
// BashToolResultMessage-specific state shaping lives in the sibling
// `bash_tool_result_message` module, matching the official file boundary.

// ─── Raw `toolUseResult` wire channel ────────────────────────────────────

/// Maps to: CC `BashTool/UI.tsx:174-198` `renderToolResultMessage` →
/// `BashToolResultMessage.tsx:64-129`. The caller resolves `timeoutMs` from
/// the last progress message (`UI.tsx:189-190`); rejection/cancel/error rows
/// never reach here (CC routes them to their own leaves;
/// `renderToolUseErrorMessage` is `FallbackToolUseErrorMessage`, `UI.tsx:200`).
///
/// The state shaping (sandbox-violation strip, cwd-reset extraction, the
/// empty-output branch) lives in `bash_tool_result_message`, matching the
/// official file boundary. CC strips `<sandbox_violations>` from stderr only;
/// the Rust executor runs bash in single-fd file mode and annotates the
/// merged stdout instead (`mod.rs` `bash_output`), so the parts helper cleans
/// both channels — a deviation that follows the executor seam, not the UI.
pub(crate) fn render_tool_result_message(
    output: &super::BashOutput,
    timeout_ms: Option<u64>,
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    let parts = super::bash_tool_result_message::bash_tool_result_message_parts(
        &super::bash_tool_result_message::BashToolResultContent {
            stdout: output.stdout.clone(),
            stderr: output.stderr.clone(),
            is_image: output.is_image,
            return_code_interpretation: output.return_code_interpretation.clone(),
            no_output_expected: output.no_output_expected,
            background_task_id: output.background_task_id.clone(),
        },
    );
    // Image results return before the timeout row (BashToolResultMessage.tsx:88-94).
    if parts.is_image {
        return vec![ToolRenderLine::new(
            parts.empty_state_text.unwrap_or_default(),
            ToolRenderTone::Inactive,
        )];
    }

    let mut lines = Vec::new();
    push_output_lines(&mut lines, &parts.stdout, ToolRenderTone::Normal, options);
    push_output_lines(&mut lines, &parts.stderr, ToolRenderTone::Error, options);
    if let Some(warning) = parts
        .cwd_reset_warning
        .as_deref()
        .filter(|warning| !warning.trim().is_empty())
    {
        lines.push(ToolRenderLine::new(
            strip_underline_ansi(warning.trim()),
            ToolRenderTone::Inactive,
        ));
    }
    if let Some(text) = parts.empty_state_text {
        lines.push(ToolRenderLine::new(text, ToolRenderTone::Inactive));
    }
    // `{timeoutMs && (...)}` — only a present, non-zero timeout renders
    // (BashToolResultMessage.tsx:122-126).
    if let Some(timeout) = timeout_ms.filter(|timeout| *timeout > 0)
        && let Some(text) = crate::components::shell::shell_time_display::shell_time_display_text(
            None,
            Some(timeout),
        )
    {
        lines.push(ToolRenderLine::new(text, ToolRenderTone::Inactive));
    }
    lines
}

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): `stdout`/`stderr`/`interrupted` are
/// required, the other ten optional (`BashTool.tsx:439-502`).
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<super::BashOutput> {
    let map = value.as_object()?;
    let optional_string = |key: &str| -> Option<Option<String>> {
        match map.get(key) {
            None => Some(None),
            Some(serde_json::Value::String(value)) => Some(Some(value.clone())),
            Some(_) => None,
        }
    };
    let optional_bool = |key: &str| -> Option<Option<bool>> {
        match map.get(key) {
            None => Some(None),
            Some(serde_json::Value::Bool(value)) => Some(Some(*value)),
            Some(_) => None,
        }
    };
    let structured_content = match map.get("structuredContent") {
        None => None,
        Some(serde_json::Value::Array(items)) => Some(items.clone()),
        Some(_) => return None,
    };
    Some(super::BashOutput {
        stdout: map.get("stdout")?.as_str()?.to_string(),
        stderr: map.get("stderr")?.as_str()?.to_string(),
        interrupted: map.get("interrupted")?.as_bool()?,
        is_image: optional_bool("isImage")?.unwrap_or(false),
        structured_content,
        raw_output_path: optional_string("rawOutputPath")?,
        background_task_id: optional_string("backgroundTaskId")?,
        backgrounded_by_user: optional_bool("backgroundedByUser")?.unwrap_or(false),
        assistant_auto_backgrounded: optional_bool("assistantAutoBackgrounded")?.unwrap_or(false),
        dangerously_disable_sandbox: optional_bool("dangerouslyDisableSandbox")?,
        no_output_expected: optional_bool("noOutputExpected")?.unwrap_or(false),
        persisted_output_path: optional_string("persistedOutputPath")?,
        persisted_output_size: match map.get("persistedOutputSize") {
            None => None,
            Some(serde_json::Value::Number(size)) => Some(size.as_u64()?),
            Some(_) => return None,
        },
        exit_code: None,
        return_code_interpretation: optional_string("returnCodeInterpretation")?,
        cwd_after: None,
        command: String::new(),
    })
}

/// Serializes [`super::BashOutput`] to CC's exact `toolUseResult` wire
/// shape — the main `call()` construction order (`BashTool.tsx:1073-1088`),
/// optionals omitted. `noOutputExpected` is always written (CC computes it
/// unconditionally via `isSilentBashCommand`); the carrier's plain-bool
/// fields omit when false, matching the common undefined path. The Rust
/// seam fields (command/duration/exit_code/cwd_after) never ride the wire.
pub(crate) fn output_to_value(output: &super::BashOutput) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("stdout".to_string(), serde_json::json!(output.stdout));
    map.insert("stderr".to_string(), serde_json::json!(output.stderr));
    map.insert(
        "interrupted".to_string(),
        serde_json::json!(output.interrupted),
    );
    if output.is_image {
        map.insert("isImage".to_string(), serde_json::json!(true));
    }
    if let Some(interpretation) = output.return_code_interpretation.as_deref() {
        map.insert(
            "returnCodeInterpretation".to_string(),
            serde_json::json!(interpretation),
        );
    }
    map.insert(
        "noOutputExpected".to_string(),
        serde_json::json!(output.no_output_expected),
    );
    if let Some(task_id) = output.background_task_id.as_deref() {
        map.insert("backgroundTaskId".to_string(), serde_json::json!(task_id));
    }
    if output.backgrounded_by_user {
        map.insert("backgroundedByUser".to_string(), serde_json::json!(true));
    }
    if output.assistant_auto_backgrounded {
        map.insert(
            "assistantAutoBackgrounded".to_string(),
            serde_json::json!(true),
        );
    }
    if let Some(disabled) = output.dangerously_disable_sandbox {
        map.insert(
            "dangerouslyDisableSandbox".to_string(),
            serde_json::json!(disabled),
        );
    }
    if let Some(path) = output.raw_output_path.as_deref() {
        map.insert("rawOutputPath".to_string(), serde_json::json!(path));
    }
    if let Some(structured) = output.structured_content.as_ref() {
        map.insert(
            "structuredContent".to_string(),
            serde_json::Value::Array(structured.clone()),
        );
    }
    if let Some(path) = output.persisted_output_path.as_deref() {
        map.insert("persistedOutputPath".to_string(), serde_json::json!(path));
    }
    if let Some(size) = output.persisted_output_size {
        map.insert("persistedOutputSize".to_string(), serde_json::json!(size));
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_tool_use_renders_sed_in_place_edit_as_file_path() {
        let collapsed = render_tool_use_message(
            "sed -i 's/foo/bar/g' src/lib.rs",
            ToolRenderOptions::default(),
        );
        assert_eq!(collapsed, "src/lib.rs");

        let macos = render_tool_use_message(
            "sed -i '' -e 's/foo/bar/' src/main.rs",
            ToolRenderOptions::default(),
        );
        assert_eq!(macos, "src/main.rs");

        let not_simple_sed = render_tool_use_message(
            "sed -n 's/foo/bar/' src/lib.rs",
            ToolRenderOptions::default(),
        );
        assert_eq!(not_simple_sed, "sed -n 's/foo/bar/' src/lib.rs");

        let glob_target = render_tool_use_message(
            "sed -i 's/foo/bar/g' src/*.rs",
            ToolRenderOptions::default(),
        );
        assert_eq!(glob_target, "sed -i 's/foo/bar/g' src/*.rs");
    }

    fn shell_output(stdout: &str, stderr: &str) -> super::super::BashOutput {
        super::super::BashOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            interrupted: false,
            is_image: false,
            structured_content: None,
            raw_output_path: None,
            background_task_id: None,
            backgrounded_by_user: false,
            assistant_auto_backgrounded: false,
            dangerously_disable_sandbox: None,
            no_output_expected: false,
            persisted_output_path: None,
            persisted_output_size: None,
            exit_code: None,
            return_code_interpretation: None,
            cwd_after: None,
            command: String::new(),
        }
    }

    #[test]
    fn bash_output_collapses_by_default_and_expands_for_transcript() {
        // Above the official 3-line fold (+ remaining==1 shows one extra).
        let stdout = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine";
        let collapsed = render_tool_result_message(
            &shell_output(stdout, ""),
            None,
            ToolRenderOptions::default(),
        );
        assert!(
            collapsed
                .iter()
                .any(|line| line.text.contains("ctrl+o to expand"))
        );
        assert!(
            !collapsed
                .iter()
                .any(|line| line.text.contains("eight\nnine"))
        );

        let expanded = render_tool_result_message(
            &shell_output(stdout, ""),
            None,
            ToolRenderOptions {
                verbose: false,
                is_transcript_mode: true,
                ..ToolRenderOptions::default()
            },
        );
        assert!(
            expanded
                .iter()
                .any(|line| line.text.contains("eight\nnine"))
        );
        assert!(
            !expanded
                .iter()
                .any(|line| line.text.contains("ctrl+o to expand"))
        );
    }

    #[test]
    fn bash_output_truncation_counts_wrapped_visual_lines() {
        // wrap_width = 30 - 10 = 20 → 180 chars ≈ 9 visual lines → fold at 3.
        let long_line = "a".repeat(180);
        let collapsed = render_tool_result_message(
            &shell_output(&long_line, ""),
            None,
            ToolRenderOptions {
                terminal_width: 30,
                ..ToolRenderOptions::default()
            },
        );

        assert_eq!(collapsed[0].text.lines().count(), 3);
        assert!(collapsed[1].text.contains("+6 lines"));
    }

    #[test]
    fn bash_output_wrap_preserves_ansi_styles_like_slice_ansi() {
        let lines = render_tool_result_message(
            &shell_output("\x1b[31mabcdefghijkl\x1b[0m", ""),
            None,
            ToolRenderOptions {
                terminal_width: 20,
                ..ToolRenderOptions::default()
            },
        );

        let wrapped = lines[0].text.lines().collect::<Vec<_>>();
        assert_eq!(
            wrapped
                .iter()
                .map(|line| strip_ansi_for_width(line))
                .collect::<Vec<_>>(),
            vec!["abcdefghij".to_string(), "kl".to_string()]
        );
        assert!(wrapped[0].starts_with("\x1b[31m"));
        assert!(wrapped[0].ends_with("\x1b[0m"));
        assert!(wrapped[1].starts_with("\x1b[31m"));
        assert!(wrapped[1].ends_with("\x1b[0m"));
    }

    #[test]
    fn bash_output_wrap_keeps_trailing_zero_width_marks_with_base_cell() {
        let lines = render_tool_result_message(
            &shell_output("\x1b[31mabcdefghie\u{301}Z\x1b[0m", ""),
            None,
            ToolRenderOptions {
                terminal_width: 20,
                ..ToolRenderOptions::default()
            },
        );

        let wrapped = lines[0].text.lines().collect::<Vec<_>>();
        assert_eq!(strip_ansi_for_width(wrapped[0]), "abcdefghie\u{301}");
        assert_eq!(strip_ansi_for_width(wrapped[1]), "Z");
        assert!(wrapped[0].starts_with("\x1b[31m"));
        assert!(wrapped[1].starts_with("\x1b[31m"));
    }

    #[test]
    fn bash_no_output_is_dim_like_official_message_response() {
        // The empty-state ladder lives in the renderer now
        // (BashToolResultMessage.tsx:107-121).
        let lines =
            render_tool_result_message(&shell_output("", ""), None, ToolRenderOptions::default());

        assert_eq!(lines[0].text, "(No output)");
        assert_eq!(lines[0].tone, ToolRenderTone::Inactive);
    }

    #[test]
    fn bash_output_renders_timeout_row_from_progress_channel() {
        // `{timeoutMs && <ShellTimeDisplay timeoutMs/>}`
        // (BashToolResultMessage.tsx:122-126).
        let lines = render_tool_result_message(
            &shell_output("ok", ""),
            Some(5000),
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "ok");
        assert_eq!(lines[1].text, "(timeout 5s)");
        assert_eq!(lines[1].tone, ToolRenderTone::Inactive);

        // Image results return before the timeout row (:88-94).
        let mut image = shell_output("data:image/png;base64,abc", "");
        image.is_image = true;
        let image_lines =
            render_tool_result_message(&image, Some(5000), ToolRenderOptions::default());
        assert_eq!(image_lines.len(), 1);
        assert_eq!(
            image_lines[0].text,
            "[Image data detected and sent to Claude]"
        );
    }

    #[test]
    fn bash_output_formats_json_like_official_output_line() {
        let lines = render_tool_result_message(
            &shell_output(r#"{"ok":true,"items":[1,2]}"#, ""),
            None,
            ToolRenderOptions {
                verbose: true,
                ..ToolRenderOptions::default()
            },
        );

        assert_eq!(
            lines[0].text,
            "{\n  \"ok\": true,\n  \"items\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(lines[0].tone, ToolRenderTone::Normal);
    }

    #[test]
    fn bash_output_keeps_js_unsafe_integer_json_unformatted() {
        let json = r#"{"id":9007199254740993}"#;
        let lines = render_tool_result_message(
            &shell_output(json, ""),
            None,
            ToolRenderOptions {
                verbose: true,
                ..ToolRenderOptions::default()
            },
        );

        assert_eq!(lines[0].text, json);
    }

    #[test]
    fn bash_output_strips_underline_ansi_and_renders_cwd_reset_as_warning() {
        // The cwd-reset warning is extracted from stderr by the renderer
        // (BashToolResultMessage.tsx:47-62), not passed pre-split.
        let lines = render_tool_result_message(
            &shell_output(
                "\x1b[4munderlined\x1b[0m",
                "error\nShell cwd was reset to /tmp/project",
            ),
            None,
            ToolRenderOptions::default(),
        );

        assert_eq!(lines[0].text, "underlined\x1b[0m");
        assert_eq!(lines[1].tone, ToolRenderTone::Error);
        assert_eq!(lines[2].text, "Shell cwd was reset to /tmp/project");
        assert_eq!(lines[2].tone, ToolRenderTone::Inactive);
    }
}
