//! UI-only port of official `TaskOutputTool/TaskOutputTool.tsx`.

use super::{Output, TaskOutput};
use crate::components::ctrl_o_to_expand::ctrl_o_to_expand_hint;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderTone,
};
use crate::tools::bash_tool;
use crate::types::message::ToolResultStatus;

pub fn render_tool_use_message(block: Option<bool>) -> String {
    if block == Some(false) {
        "non-blocking".to_string()
    } else {
        String::new()
    }
}

/// Maps to CC `TaskOutputTool.tsx:377-382`:
/// `if (!input.task_id) return null; return <Text dimColor> {input.task_id}</Text>`.
///
/// The guard is plain truthiness, so a whitespace-only id renders in CC; the
/// earlier `.trim()` here dropped it. The leading space belongs to the JSX
/// literal and is applied by the renderer, not stored in the tag.
///
/// Seam: CC reads `input.task_id` only. The alias set is this port's existing
/// tolerance for the `taskId`/`shell_id` spellings the BashOutput alias carries.
pub fn render_tool_use_tag(input: &serde_json::Value) -> Option<String> {
    crate::components::messages::user_tool_result_message::utils::first_string(
        input,
        &["task_id", "taskId", "shell_id", "shellId"],
    )
    .filter(|value| !value.is_empty())
}

/// The renderer's view of the raw `toolUseResult`. TaskOutputTool defines no
/// outputSchema, so CC's `tool.outputSchema?.safeParse` short-circuits and
/// `TaskOutputResultDisplay` receives the raw value — a string is
/// `jsonParse`d first (`TaskOutputTool.tsx:436-437`). Field access is
/// unchecked in CC; absent fields interpolate empty.
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<Output> {
    let parsed;
    let value = match value {
        serde_json::Value::String(text) => {
            parsed = serde_json::from_str::<serde_json::Value>(text).ok()?;
            &parsed
        }
        other => other,
    };
    let map = value.as_object()?;
    let string = |target: &serde_json::Map<String, serde_json::Value>, key: &str| {
        target
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    };
    let task = match map.get("task") {
        None | Some(serde_json::Value::Null) => None,
        Some(task) => {
            let task = task.as_object()?;
            Some(TaskOutput {
                task_id: string(task, "task_id").unwrap_or_default(),
                task_type: string(task, "task_type").unwrap_or_default(),
                status: string(task, "status").unwrap_or_default(),
                description: string(task, "description").unwrap_or_default(),
                output: string(task, "output").unwrap_or_default(),
                exit_code: task
                    .get("exitCode")
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok()),
                error: string(task, "error"),
                prompt: string(task, "prompt"),
                result: string(task, "result"),
            })
        }
    };
    Some(Output {
        retrieval_status: string(map, "retrieval_status").unwrap_or_default(),
        task,
    })
}

/// Serializes [`Output`] to CC's exact `toolUseResult` wire shape — the
/// `getTaskOutputData` construction (`TaskOutputTool.tsx:87-131`): the base
/// five fields, then the type-specific tail (local_bash always writes
/// `exitCode`, possibly null; agent/remote optionals omit when absent).
pub(crate) fn output_to_value(output: &Output) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "retrieval_status".to_string(),
        serde_json::Value::String(output.retrieval_status.clone()),
    );
    let task_value = match output.task.as_ref() {
        None => serde_json::Value::Null,
        Some(task) => {
            let mut object = serde_json::Map::new();
            object.insert("task_id".to_string(), serde_json::json!(task.task_id));
            object.insert("task_type".to_string(), serde_json::json!(task.task_type));
            object.insert("status".to_string(), serde_json::json!(task.status));
            object.insert(
                "description".to_string(),
                serde_json::json!(task.description),
            );
            object.insert("output".to_string(), serde_json::json!(task.output));
            if task.task_type == "local_bash" {
                object.insert(
                    "exitCode".to_string(),
                    task.exit_code
                        .map_or(serde_json::Value::Null, |code| serde_json::json!(code)),
                );
            }
            if let Some(prompt) = task.prompt.as_ref() {
                object.insert("prompt".to_string(), serde_json::json!(prompt));
            }
            if let Some(result) = task.result.as_ref() {
                object.insert("result".to_string(), serde_json::json!(result));
            }
            if let Some(error) = task.error.as_ref() {
                object.insert("error".to_string(), serde_json::json!(error));
            }
            serde_json::Value::Object(object)
        }
    };
    map.insert("task".to_string(), task_value);
    serde_json::Value::Object(map)
}

/// Maps to: CC `TaskOutputTool.tsx:422-562` `TaskOutputResultDisplay`.
pub(crate) fn render_tool_result_message(
    raw_output: Option<&serde_json::Value>,
    fallback: &str,
    status: ToolResultStatus,
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    let Some(output) = raw_output.and_then(parse_output) else {
        return Vec::new();
    };
    let Some(task) = output.task.as_ref() else {
        return vec![ToolRenderLine::new(
            "No task output available",
            ToolRenderTone::Inactive,
        )];
    };

    match task.task_type.as_str() {
        // CC builds a synthetic BashOut and hands it to BashToolResultMessage
        // (TaskOutputTool.tsx:450-459): only stdout, stderr: '', isImage:
        // false, dangerouslyDisableSandbox: true, and returnCodeInterpretation
        // = task.error are set; there is no timeoutMs (no progress channel).
        "local_bash" => bash_tool::ui::render_tool_result_message(
            &crate::tools::bash_tool::BashOutput {
                stdout: task.output.clone(),
                stderr: String::new(),
                interrupted: false,
                is_image: false,
                structured_content: None,
                raw_output_path: None,
                background_task_id: None,
                backgrounded_by_user: false,
                assistant_auto_backgrounded: false,
                dangerously_disable_sandbox: Some(true),
                no_output_expected: false,
                persisted_output_path: None,
                persisted_output_size: None,
                exit_code: None,
                return_code_interpretation: task.error.clone(),
                cwd_after: None,
                command: String::new(),
            },
            None,
            options,
        ),
        "local_agent" => render_local_agent_lines(&output, task, options),
        // Default rendering: `task.output.slice(0, 500)` in every mode —
        // CC has no verbose full-text branch here.
        _ => {
            let mut lines = vec![ToolRenderLine::new(
                format!("  {} [{}]", task.description, task.status),
                ToolRenderTone::Normal,
            )];
            if !task.output.is_empty() {
                lines.push(ToolRenderLine::new(
                    format!("    {}", task.output.chars().take(500).collect::<String>()),
                    ToolRenderTone::Normal,
                ));
            }
            lines
        }
    }
}

/// Maps to: CC `TaskOutputTool.tsx:462-526` — the local_agent branch.
fn render_local_agent_lines(
    output: &Output,
    task: &TaskOutput,
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    // `task.result ? countCharInString(task.result, '\n') + 1 : 0`.
    let line_count = task
        .result
        .as_deref()
        .filter(|result| !result.is_empty())
        .map_or(0, |result| {
            result.chars().filter(|ch| *ch == '\n').count() + 1
        });

    if output.retrieval_status == "success" {
        if options.show_full() {
            let mut lines = vec![ToolRenderLine::new(
                format!("{} ({line_count} lines)", task.description),
                ToolRenderTone::Normal,
            )];
            if let Some(prompt) = task.prompt.as_deref().filter(|prompt| !prompt.is_empty()) {
                lines.push(ToolRenderLine::new("Prompt:", ToolRenderTone::Success));
                lines.push(ToolRenderLine::new(
                    prompt.to_string(),
                    ToolRenderTone::Normal,
                ));
            }
            if let Some(result) = task.result.as_deref().filter(|result| !result.is_empty()) {
                lines.push(ToolRenderLine::new("Response:", ToolRenderTone::Success));
                lines.push(ToolRenderLine::new(
                    result.to_string(),
                    ToolRenderTone::Normal,
                ));
            }
            if let Some(error) = task.error.as_deref().filter(|error| !error.is_empty()) {
                lines.push(ToolRenderLine::new("Error:", ToolRenderTone::Error));
                lines.push(ToolRenderLine::new(
                    error.to_string(),
                    ToolRenderTone::Error,
                ));
            }
            return lines;
        }
        return vec![ToolRenderLine::new(
            format!("Read output {}", ctrl_o_to_expand_hint()),
            ToolRenderTone::Inactive,
        )];
    }

    // timeout, a still-running task, and not_ready all show the waiting
    // copy (TaskOutputTool.tsx:505-519).
    if matches!(output.retrieval_status.as_str(), "timeout" | "not_ready")
        || task.status == "running"
    {
        return vec![ToolRenderLine::new(
            "Task is still running…",
            ToolRenderTone::Inactive,
        )];
    }

    vec![ToolRenderLine::new(
        "Task not ready",
        ToolRenderTone::Inactive,
    )]
}

/// Maps to CC `TaskOutputTool.tsx:369-375` `renderToolUseMessage`, which reads
/// ONLY `block` (`const { block = true } = input`) and never the task id — the
/// id is the separate `renderToolUseTag` member above.
///
/// This used to join the two into `"non-blocking · {task_id}"`, which
/// `assistant_tool_use_display_parts` then reverse-parsed back apart. That is
/// the reverse-parse disease Cut E removed from the Agent row: CC composes the
/// row from two independent sources and never recovers one from the other.
///
/// The `None` return is NOT part of `renderToolUseMessage`, which never returns
/// null. It stands in for a layer above: CC's schema is
/// `z.strictObject({ task_id: z.string(), … })` (`TaskOutputTool.tsx:30-43`), so
/// `task_id` is required, and a row failing `inputSchema.safeParse` renders
/// nothing at all (`AssistantToolUseMessage.tsx:88-89`, `:145-150`).
///
/// Audit follow-up 11 has since landed that parse as
/// `assistant_tool_use_message.rs#assistant_tool_use_input_parses`, so on the
/// transcript row this check is now redundant — but it is NOT dead, and it is
/// not equivalent:
/// - the gate only runs inside `AssistantToolUseMessage`; the other callers of
///   `render_tool_use_message` (`message.rs`, `grouped_tool_use_content.rs`)
///   have no gate, matching the CC sites they map to;
/// - the gate is keyed on names CC resolves to a tool, so the invented
///   `bashoutput` alias reaches this function ungated;
/// - `first_string` also accepts the `taskId` / `shell_id` / `shellId`
///   spellings, which CC's strict schema rejects outright.
///
/// `first_string` accepts `""` and rejects a missing or non-string value, which
/// is exactly `z.string()`'s boundary.
pub(crate) fn task_output_tool_use_summary(input: &serde_json::Value) -> Option<String> {
    crate::components::messages::user_tool_result_message::utils::first_string(
        input,
        &["task_id", "taskId", "shell_id", "shellId"],
    )?;
    Some(render_tool_use_message(
        crate::components::messages::user_tool_result_message::utils::first_bool(input, &["block"]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_output_tool_use_message_and_tag_match_official_subset() {
        assert_eq!(render_tool_use_message(Some(false)), "non-blocking");
        assert_eq!(render_tool_use_message(Some(true)), "");
        // `const { block = true }` — the default applies to absence only.
        assert_eq!(render_tool_use_message(None), "");

        assert_eq!(
            render_tool_use_tag(&serde_json::json!({ "task_id": "task-7" })),
            Some("task-7".to_string())
        );
        assert_eq!(
            render_tool_use_tag(&serde_json::json!({ "task_id": "" })),
            None
        );
        assert_eq!(render_tool_use_tag(&serde_json::json!({})), None);
        // `if (!input.task_id)` is truthiness, so whitespace survives in CC.
        assert_eq!(
            render_tool_use_tag(&serde_json::json!({ "task_id": " " })),
            Some(" ".to_string())
        );
    }

    #[test]
    fn task_output_local_agent_success_uses_expand_copy_when_not_verbose() {
        let raw = serde_json::json!({
            "retrieval_status": "success",
            "task": {
                "task_id": "task-7",
                "task_type": "local_agent",
                "status": "completed",
                "description": "Review auth",
                "output": "done",
                "result": "done",
            },
        });
        let lines = render_tool_result_message(
            Some(&raw),
            "",
            ToolResultStatus::Success,
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "Read output (ctrl+o to expand)");
        assert_eq!(lines[0].tone, ToolRenderTone::Inactive);
    }

    #[test]
    fn task_output_running_agent_uses_official_waiting_copy() {
        let raw = serde_json::json!({
            "retrieval_status": "not_ready",
            "task": {
                "task_id": "task-7",
                "task_type": "local_agent",
                "status": "running",
                "description": "Review auth",
                "output": "",
            },
        });
        let lines = render_tool_result_message(
            Some(&raw),
            "",
            ToolResultStatus::Success,
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "Task is still running…");
    }

    #[test]
    fn task_output_null_task_and_default_branch_match_official_shapes() {
        // `!result.task` → the dim "No task output available" line.
        let null_task = serde_json::json!({"retrieval_status": "success", "task": null});
        let lines = render_tool_result_message(
            Some(&null_task),
            "",
            ToolResultStatus::Success,
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "No task output available");

        // The default branch always slices to 500 chars, even in verbose,
        // and always renders the [status] bracket.
        let long_output = "x".repeat(600);
        let raw = serde_json::json!({
            "retrieval_status": "success",
            "task": {
                "task_id": "task-9",
                "task_type": "in_process",
                "status": "completed",
                "description": "Teammate task",
                "output": long_output,
            },
        });
        let verbose = render_tool_result_message(
            Some(&raw),
            "",
            ToolResultStatus::Success,
            ToolRenderOptions {
                verbose: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(verbose[0].text, "  Teammate task [completed]");
        assert_eq!(verbose[1].text.len(), 4 + 500);
    }

    #[test]
    fn task_output_round_trips_local_bash_wire_shape() {
        let output = Output {
            retrieval_status: "success".to_string(),
            task: Some(TaskOutput {
                task_id: "task-1".to_string(),
                task_type: "local_bash".to_string(),
                status: "completed".to_string(),
                description: "build".to_string(),
                output: "build ok".to_string(),
                exit_code: Some(0),
                error: None,
                prompt: None,
                result: None,
            }),
        };
        let value = output_to_value(&output);
        // local_bash always writes exitCode, null when absent.
        assert_eq!(value["task"]["exitCode"], serde_json::json!(0));
        assert_eq!(parse_output(&value).unwrap(), output);
    }
}
