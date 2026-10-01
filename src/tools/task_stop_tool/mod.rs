//! TaskStop tool metadata and UI.
//!
//! Maps to:
//! - CC `tools/TaskStopTool/TaskStopTool.ts`
//! - CC `tools/TaskStopTool/prompt.ts`
//! - CC `tools/TaskStopTool/UI.tsx`
//!
//! Execution lives in this module, dispatched from `services/tools/tool_execution.rs`.

pub mod prompt;
pub mod ui;

/// Maps to CC `TaskStopTool.inputSchema`.
/// Maps to: CC `TaskStopTool.ts:10-19` `inputSchema`. Both fields are optional;
/// `shell_id` exists only for the deprecated KillShell alias.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "task_id",
                zod::string()
                    .optional()
                    .describe("The ID of the background task to stop"),
            ),
            (
                "shell_id",
                zod::string()
                    .optional()
                    .describe("Deprecated: use task_id instead"),
            ),
        ])
    })
}

pub fn task_stop_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::TASK_STOP_TOOL_NAME.to_string(),
        // Maps to: CC `TaskStopTool.aliases` — KillShell.
        aliases: vec!["KillShell".to_string()],
        description: prompt::DESCRIPTION.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/TaskStopTool/TaskStopTool.ts` `export type Output` (:37) —
/// outputSchema (:22-34).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) message: String,
    pub(crate) task_id: String,
    pub(crate) task_type: String,
    pub(crate) command: Option<String>,
}

/// Stop a running task through the shared Tool/SDK owner.
/// Maps to CC `tools/TaskStopTool/TaskStopTool.ts:107-132` delegating to
/// `tasks/stopTask.ts:38-73`.
pub(crate) async fn task_stop_output(
    input: &serde_json::Value,
    app_store: Option<&crate::state::store::AppStore>,
) -> Result<Output, String> {
    let Some(task_id) = input
        .get("task_id")
        .or_else(|| input.get("shell_id"))
        .and_then(|value| value.as_str())
    else {
        return Err("Missing required parameter: task_id".to_string());
    };

    let result = crate::tasks::stop_task::stop_task(task_id, app_store)
        .await
        .map_err(|error| error.to_string())?;
    let command = result
        .command
        .clone()
        .unwrap_or_else(|| "undefined".to_string());
    Ok(Output {
        message: format!("Successfully stopped task: {} ({command})", result.task_id),
        task_id: result.task_id,
        task_type: result.task_type,
        command: result.command,
    })
}

fn task_stop_error_result(error: String) -> crate::tool::ToolResult {
    crate::tool::ToolResult {
        data: crate::tool::ToolOutput::Composed {
            content: error,
            status: crate::types::message::ToolResultStatus::Error,
        },
        new_messages: Vec::new(),
    }
}

/// Behavioral half of CC `TaskStopTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TaskStopTool;

impl crate::tool::ToolCall for TaskStopTool {
    fn name(&self) -> &'static str {
        "TaskStop"
    }

    /// Maps to: CC `TaskStopTool.ts:95-97` `async prompt() { return
    /// DESCRIPTION }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::DESCRIPTION.to_string()
    }

    /// Maps to: CC `TaskStopTool.isConcurrencySafe()` (TaskStopTool.ts:54) — true.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["KillShell"]
    }

    /// Maps to: CC `TaskStopTool.ts:46` `userFacingName: () =>
    /// process.env.USER_TYPE === 'ant' ? '' : 'Stop Task'`.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        if crate::utils::process_env::var("USER_TYPE").as_deref() == Some("ant") {
            String::new()
        } else {
            "Stop Task".to_string()
        }
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("kill a running background task")
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("task_id")
            .or_else(|| args.get("shell_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        let Some(task_id) = args
            .get("task_id")
            .or_else(|| args.get("shell_id"))
            .and_then(serde_json::Value::as_str)
        else {
            return crate::tool::ValidationResult::error("Missing required parameter: task_id", 1);
        };
        let app_store = context
            .app_store
            .tasks_store
            .as_ref()
            .or(context.app_store.store.as_ref());
        let Some(task) = crate::tasks::stop_task::lookup_task(task_id, app_store) else {
            return crate::tool::ValidationResult::error(
                format!("No task found with ID: {task_id}"),
                1,
            );
        };
        if task.status != "running" {
            return crate::tool::ValidationResult::error(
                format!("Task {task_id} is not running (status: {})", task.status),
                3,
            );
        }
        crate::tool::ValidationResult::Ok
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let app_store = context
                .app_store
                .tasks_store
                .clone()
                .or_else(|| context.app_store.store.clone());
            match task_stop_output(args, app_store.as_ref()).await {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::TaskStop(output),
                    new_messages: Vec::new(),
                },
                Err(error) => task_stop_error_result(error),
            }
        })
    }

    /// Maps to: CC `tools/TaskStopTool/TaskStopTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:98-104).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::TaskStop(output) => (
                ui::output_to_value(output).to_string(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>TaskStop returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording TaskStopTool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TaskStop(output) => Some(ui::output_to_value(output)),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_stop_tool_schema_matches_official_input_shape() {
        let schema = task_stop_tool_schema();
        assert_eq!(schema.name, "TaskStop");
        // Both fields are optional, so zod emits no `required` key.
        assert_eq!(schema.input_schema.get("required"), None);
        for key in ["task_id", "shell_id"] {
            assert!(
                schema
                    .input_schema
                    .pointer(&format!("/properties/{key}"))
                    .is_some()
            );
        }
        assert!(
            schema
                .description
                .contains("Stops a running background task")
        );
    }

    #[test]
    fn task_stop_behavior_metadata_matches_official_consumers() {
        let tool = TaskStopTool;
        assert_eq!(
            crate::tool::ToolCall::search_hint(&tool),
            Some("kill a running background task")
        );
        assert!(crate::tool::ToolCall::should_defer(&tool));
        assert_eq!(crate::tool::ToolCall::max_result_size_chars(&tool), 100_000);
        assert_eq!(
            crate::tool::ToolCall::to_auto_classifier_input(
                &tool,
                &serde_json::json!({"shell_id":"task-1"}),
            ),
            "task-1"
        );
        assert_eq!(
            crate::tool::ToolCall::validate_input(
                &tool,
                &serde_json::json!({}),
                &crate::tool::ToolUseContext::default(),
            ),
            crate::tool::ValidationResult::error("Missing required parameter: task_id", 1,)
        );
        assert_eq!(
            crate::tool::ToolCall::validate_input(
                &tool,
                &serde_json::json!({"task_id":"missing-task"}),
                &crate::tool::ToolUseContext::default(),
            ),
            crate::tool::ValidationResult::error("No task found with ID: missing-task", 1,)
        );
    }
}
