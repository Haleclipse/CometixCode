//! UI-only helpers for official `PowerShellTool/UI.tsx`.

/// Maps to: CC `tools/PowerShellTool/PowerShellTool.tsx:463-474`
/// `getToolUseSummary` — byte-identical to BashTool's (`:720-730`): `null`
/// without a command, else the `description` when present, else the truncated
/// command.
pub fn get_tool_use_summary(input: Option<&serde_json::Value>) -> Option<String> {
    crate::tools::bash_tool::ui::get_tool_use_summary(input)
}

use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderTone,
};
use crate::tools::bash_tool;

const MAX_COMMAND_DISPLAY_LINES: usize = 2;
const MAX_COMMAND_DISPLAY_CHARS: usize = 160;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PowerShellResultView {
    pub stdout: String,
    pub stderr: String,
    pub interrupted: bool,
    pub is_image: bool,
    pub background_task_id: Option<String>,
    pub return_code_interpretation: Option<String>,
}

pub fn render_tool_use_message(command: Option<&str>, verbose: bool) -> Option<String> {
    let command = command?;
    if command.is_empty() {
        return None;
    }
    if verbose {
        return Some(command.to_string());
    }

    let lines = command.lines().collect::<Vec<_>>();
    let needs_line_truncation = lines.len() > MAX_COMMAND_DISPLAY_LINES;
    let needs_char_truncation = command.chars().count() > MAX_COMMAND_DISPLAY_CHARS;
    if !needs_line_truncation && !needs_char_truncation {
        return Some(command.to_string());
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
    Some(format!("{}…", truncated.trim()))
}

pub fn map_tool_result_content(view: &PowerShellResultView) -> String {
    if view.is_image {
        return "[Image data detected and sent to Claude]".to_string();
    }

    let mut parts = Vec::new();
    if !view.stdout.is_empty() {
        parts.push(view.stdout.clone());
    }
    if !view.stderr.trim().is_empty() {
        parts.push(view.stderr.trim().to_string());
    }
    if !parts.is_empty() {
        return parts.join("\n");
    }

    power_shell_empty_result_text(view).unwrap_or_else(|| "(No output)".to_string())
}

// ─── Raw `toolUseResult` wire channel ────────────────────────────────────

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): `stdout`/`stderr`/`interrupted` are
/// required, the other seven optional (`PowerShellTool.tsx:310-353`).
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<super::PowerShellOutput> {
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
    Some(super::PowerShellOutput {
        stdout: map.get("stdout")?.as_str()?.to_string(),
        stderr: map.get("stderr")?.as_str()?.to_string(),
        interrupted: map.get("interrupted")?.as_bool()?,
        return_code_interpretation: optional_string("returnCodeInterpretation")?,
        is_image: optional_bool("isImage")?.unwrap_or(false),
        persisted_output_path: optional_string("persistedOutputPath")?,
        persisted_output_size: match map.get("persistedOutputSize") {
            None => None,
            Some(serde_json::Value::Number(size)) => Some(size.as_u64()?),
            Some(_) => return None,
        },
        background_task_id: optional_string("backgroundTaskId")?,
        backgrounded_by_user: optional_bool("backgroundedByUser")?,
        assistant_auto_backgrounded: optional_bool("assistantAutoBackgrounded")?,
    })
}

/// Serializes [`super::PowerShellOutput`] to CC's exact `toolUseResult` wire
/// shape — the main `call()` construction order (`PowerShellTool.tsx:841-849`),
/// with the background-branch keys after it (:709-717); optionals omitted.
/// `isImage` omits when false, matching the Bash channel's treatment of the
/// carrier's plain-bool fields. The Rust seam fields (command/duration_ms)
/// never ride the wire.
pub(crate) fn output_to_value(output: &super::PowerShellOutput) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("stdout".to_string(), serde_json::json!(output.stdout));
    map.insert("stderr".to_string(), serde_json::json!(output.stderr));
    map.insert(
        "interrupted".to_string(),
        serde_json::json!(output.interrupted),
    );
    if let Some(interpretation) = output.return_code_interpretation.as_deref() {
        map.insert(
            "returnCodeInterpretation".to_string(),
            serde_json::json!(interpretation),
        );
    }
    if output.is_image {
        map.insert("isImage".to_string(), serde_json::json!(true));
    }
    if let Some(path) = output.persisted_output_path.as_deref() {
        map.insert("persistedOutputPath".to_string(), serde_json::json!(path));
    }
    if let Some(size) = output.persisted_output_size {
        map.insert("persistedOutputSize".to_string(), serde_json::json!(size));
    }
    if let Some(task_id) = output.background_task_id.as_deref() {
        map.insert("backgroundTaskId".to_string(), serde_json::json!(task_id));
    }
    if let Some(by_user) = output.backgrounded_by_user {
        map.insert("backgroundedByUser".to_string(), serde_json::json!(by_user));
    }
    if let Some(auto) = output.assistant_auto_backgrounded {
        map.insert(
            "assistantAutoBackgrounded".to_string(),
            serde_json::json!(auto),
        );
    }
    serde_json::Value::Object(map)
}

/// Maps to: CC `PowerShellTool/UI.tsx:103-166` `renderToolResultMessage`.
/// Unlike Bash there is no sandbox-violation / cwd-reset stripping, and the
/// empty-state ladder is `backgroundTaskId` → `interrupted` ("Interrupted") →
/// `returnCodeInterpretation || '(No output)'` (JS truthiness: empty strings
/// fall through). The caller resolves `timeoutMs` from the last progress
/// message (:118-119); `renderToolUseErrorMessage` is
/// `FallbackToolUseErrorMessage` (:168).
pub(crate) fn render_tool_result_message(
    output: &super::PowerShellOutput,
    timeout_ms: Option<u64>,
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    // Image results return before the timeout row (UI.tsx:129-135).
    if output.is_image {
        return vec![ToolRenderLine::new(
            "[Image data detected and sent to Claude]",
            ToolRenderTone::Inactive,
        )];
    }

    let mut lines = Vec::new();
    bash_tool::ui::push_output_lines(&mut lines, &output.stdout, ToolRenderTone::Normal, options);
    bash_tool::ui::push_output_lines(&mut lines, &output.stderr, ToolRenderTone::Error, options);
    if output.stdout.is_empty() && output.stderr.trim().is_empty() {
        let text = if output
            .background_task_id
            .as_deref()
            .is_some_and(|task_id| !task_id.is_empty())
        {
            "Running in the background (↓ to manage)".to_string()
        } else if output.interrupted {
            "Interrupted".to_string()
        } else {
            output
                .return_code_interpretation
                .as_deref()
                .filter(|interpretation| !interpretation.is_empty())
                .unwrap_or("(No output)")
                .to_string()
        };
        lines.push(ToolRenderLine::new(text, ToolRenderTone::Inactive));
    }
    // `{timeoutMs ? (...) : null}` (UI.tsx:159-163).
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

pub fn parse_result_view(value: &serde_json::Value) -> Option<PowerShellResultView> {
    let map = value.as_object()?;
    Some(PowerShellResultView {
        stdout: map
            .get("stdout")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        stderr: map
            .get("stderr")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        interrupted: map
            .get("interrupted")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        is_image: map
            .get("isImage")
            .or_else(|| map.get("is_image"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        background_task_id: map
            .get("backgroundTaskId")
            .or_else(|| map.get("background_task_id"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        return_code_interpretation: map
            .get("returnCodeInterpretation")
            .or_else(|| map.get("return_code_interpretation"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
    })
}

fn power_shell_empty_result_text(view: &PowerShellResultView) -> Option<String> {
    if view.background_task_id.is_some() {
        return Some("Running in the background (↓ to manage)".to_string());
    }
    if view.interrupted {
        return Some("Interrupted".to_string());
    }
    view.return_code_interpretation
        .clone()
        .or_else(|| Some("(No output)".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_tool_use_message_matches_official_truncation() {
        assert_eq!(render_tool_use_message(None, false), None);
        assert_eq!(
            render_tool_use_message(Some("Get-ChildItem"), false).as_deref(),
            Some("Get-ChildItem")
        );
        assert_eq!(
            render_tool_use_message(Some("one\ntwo\nthree"), false).as_deref(),
            Some("one\ntwo…")
        );
        assert_eq!(
            render_tool_use_message(Some("one\ntwo\nthree"), true).as_deref(),
            Some("one\ntwo\nthree")
        );
    }

    fn ps_output(stdout: &str, stderr: &str) -> super::super::PowerShellOutput {
        super::super::PowerShellOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            interrupted: false,
            return_code_interpretation: None,
            is_image: false,
            persisted_output_path: None,
            persisted_output_size: None,
            background_task_id: None,
            backgrounded_by_user: None,
            assistant_auto_backgrounded: None,
        }
    }

    #[test]
    fn powershell_result_lines_match_official_empty_image_and_background_rows() {
        // Maps to: CC `PowerShellTool/UI.tsx:129-158` — image early return,
        // then the empty ladder backgroundTaskId → interrupted → rci.
        let mut image = ps_output("", "");
        image.is_image = true;
        assert_eq!(
            render_tool_result_message(&image, None, ToolRenderOptions::default())[0].text,
            "[Image data detected and sent to Claude]"
        );

        let mut background = ps_output("", "");
        background.background_task_id = Some("ps-1".to_string());
        let background_lines =
            render_tool_result_message(&background, None, ToolRenderOptions::default());
        assert_eq!(
            background_lines[0].text,
            "Running in the background (↓ to manage)"
        );
        assert_eq!(background_lines[0].tone, ToolRenderTone::Inactive);

        let mut interrupted = ps_output("", "");
        interrupted.interrupted = true;
        interrupted.return_code_interpretation = Some("should not show".to_string());
        assert_eq!(
            render_tool_result_message(&interrupted, None, ToolRenderOptions::default())[0].text,
            "Interrupted"
        );

        let mut interpreted = ps_output("", "");
        interpreted.return_code_interpretation = Some("No matches found".to_string());
        assert_eq!(
            render_tool_result_message(&interpreted, None, ToolRenderOptions::default())[0].text,
            "No matches found"
        );

        // `{timeoutMs ? <ShellTimeDisplay/> : null}` (UI.tsx:159-163).
        let lines = render_tool_result_message(
            &ps_output("ok", ""),
            Some(5000),
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "ok");
        assert_eq!(lines[1].text, "(timeout 5s)");

        let view = PowerShellResultView {
            return_code_interpretation: Some("No matches found".to_string()),
            ..PowerShellResultView::default()
        };
        assert_eq!(map_tool_result_content(&view), "No matches found");
    }

    #[test]
    fn powershell_output_round_trips_through_wire_shape() {
        let mut output = ps_output("out", "err");
        output.return_code_interpretation = Some("meaning".to_string());
        output.background_task_id = Some("ps-9".to_string());
        output.backgrounded_by_user = Some(true);

        let wire = output_to_value(&output);
        assert_eq!(
            wire.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec![
                "stdout",
                "stderr",
                "interrupted",
                "returnCodeInterpretation",
                "backgroundTaskId",
                "backgroundedByUser"
            ]
        );

        let parsed = parse_output(&wire).expect("wire shape parses");
        assert_eq!(parsed.stdout, "out");
        assert_eq!(parsed.stderr, "err");
        assert_eq!(
            parsed.return_code_interpretation.as_deref(),
            Some("meaning")
        );
        assert_eq!(parsed.background_task_id.as_deref(), Some("ps-9"));
        assert_eq!(parsed.backgrounded_by_user, Some(true));
        // Rejected shapes: missing required keys / wrong types.
        assert!(parse_output(&serde_json::json!({"stdout": "x", "stderr": ""})).is_none());
        assert!(
            parse_output(&serde_json::json!({
                "stdout": "x", "stderr": "", "interrupted": "no"
            }))
            .is_none()
        );
    }
}
