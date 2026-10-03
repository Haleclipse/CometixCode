//! Incremental port of official `TaskGetTool/TaskGetTool.ts`.
//! `renderToolUseMessage()` returns `null` and the tool does not define a
//! visible success result renderer; recorded successes are therefore hidden in
//! the main transcript, while model-facing content remains session data only.

pub mod prompt;
pub mod ui;

/// Maps to: CC `TaskGetTool` metadata.
/// Maps to: CC `TaskGetTool.ts:13-17` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![(
            "taskId",
            zod::string().describe("The ID of the task to retrieve"),
        )])
    })
}

pub fn task_get_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::TASK_GET_TOOL_NAME.to_string(),
        description: prompt::PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/TaskGetTool/TaskGetTool.ts` outputSchema (:20).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskGetTaskOutput {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) description: String,
    pub(crate) status: String,
    pub(crate) blocks: Vec<String>,
    pub(crate) blocked_by: Vec<String>,
}

/// CC `tools/TaskGetTool/TaskGetTool.ts` outputSchema (:20).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskGetOutput {
    pub(crate) task: Option<TaskGetTaskOutput>,
}

/// Fetch one task from the disk TodoV2 store.
/// Maps to: CC `tools/TaskGetTool/TaskGetTool.ts` `call` (:73), which
/// delegates to `utils/tasks.ts` `getTask` (:310).
pub(crate) fn task_get_output(input: &serde_json::Value) -> TaskGetOutput {
    // CC destructures only `taskId` — the strict schema admits no alias.
    let task_id = input
        .get("taskId")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let list_id = crate::utils::tasks::get_task_list_id();
    let task = crate::utils::tasks::get_task(&list_id, task_id).map(|task| TaskGetTaskOutput {
        id: task.id,
        subject: task.subject,
        description: task.description,
        status: task.status,
        blocks: task.blocks,
        blocked_by: task.blocked_by,
    });
    TaskGetOutput { task }
}

/// Serializes [`TaskGetOutput`] to CC's `toolUseResult` wire shape — the
/// `call()` construction order (`TaskGetTool.ts:73-96`), camelCase keys.
pub(crate) fn output_to_value(output: &TaskGetOutput) -> serde_json::Value {
    match output.task.as_ref() {
        None => serde_json::json!({ "task": null }),
        Some(task) => serde_json::json!({
            "task": {
                "id": task.id,
                "subject": task.subject,
                "description": task.description,
                "status": task.status,
                "blocks": task.blocks,
                "blockedBy": task.blocked_by,
            }
        }),
    }
}

fn task_get_view(task: &TaskGetTaskOutput) -> ui::TaskGetView {
    ui::TaskGetView {
        id: task.id.clone(),
        subject: task.subject.clone(),
        description: task.description.clone(),
        status: task.status.clone(),
        blocked_by: task.blocked_by.clone(),
        blocks: task.blocks.clone(),
    }
}

/// Behavioral half of CC `TaskGetTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TaskGetTool;

impl crate::tool::ToolCall for TaskGetTool {
    fn name(&self) -> &'static str {
        "TaskGet"
    }

    /// Maps to: CC `TaskGetTool.ts:45-47` `async prompt() { return PROMPT }`
    /// — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::PROMPT.to_string()
    }

    /// Maps to: CC `TaskGetTool.ts:58-60` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        crate::utils::tasks::is_todo_v2_enabled()
    }

    /// Maps to: CC `TaskGetTool.isConcurrencySafe(...)`.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `TaskGetTool.ts:64-66` `isReadOnly()`.
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `TaskGetTool.ts:40` `searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("retrieve a task by ID")
    }

    /// Maps to: CC `TaskGetTool.ts:57` `shouldDefer: true`.
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `TaskGetTool.ts:67-69` `toAutoClassifierInput` —
    /// `input.taskId`.
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("taskId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        _context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let _ = request;
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::TaskGet(task_get_output(args)),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/TaskGetTool/TaskGetTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:99-127).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::TaskGet(output) => {
                let view = output.task.as_ref().map(task_get_view);
                (
                    ui::map_tool_result_content(view.as_ref()),
                    crate::types::message::ToolResultStatus::Success,
                )
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>TaskGet returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording this tool's `Output` (the `call()` data) as the
    /// message's `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TaskGet(output) => Some(output_to_value(output)),
            _ => None,
        }
    }

    // CC `TaskGetTool` defines no `renderToolResultMessage` — the render
    // layer hides success rows by name (`success_tool_result_is_nonvisual`).
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn task_get_schema_matches_official_input_shape() {
        let schema = super::task_get_tool_schema();
        assert_eq!(schema.name, "TaskGet");
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!(["taskId"]))
        );
        assert!(schema.description.contains("retrieve a task by its ID"));
    }

    #[tokio::test]
    async fn task_get_tool_call_returns_official_output_schema_and_model_copy() {
        use crate::tool::ToolCall;

        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("tool-list");
        {
            let mut tasks = crate::utils::tasks::TASK_TOOL_STORE.lock().unwrap();
            tasks.clear();
            tasks.push(crate::utils::tasks::TaskRecord {
                id: "7".to_string(),
                subject: "Fix auth".to_string(),
                description: "Patch token refresh".to_string(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: vec!["9".to_string()],
                blocked_by: vec!["3".to_string()],
                metadata: None,
            });
        }

        let args = serde_json::json!({"taskId": "7"});
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-task-get".to_string(),
            "toolu_task_get".to_string(),
            "TaskGet".to_string(),
            "7".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = super::TaskGetTool;
        let result = tool
            .call(
                &args,
                &request,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;

        let crate::tool::ToolOutput::TaskGet(output) = result.data else {
            panic!("TaskGet should return its official ToolOutput variant");
        };
        let task = output.task.as_ref().expect("task should exist");
        assert_eq!(task.id, "7");
        assert_eq!(task.blocked_by, vec!["3".to_string()]);
        assert_eq!(task.blocks, vec!["9".to_string()]);

        let data = crate::tool::ToolOutput::TaskGet(output);
        let (content, status) =
            tool.map_tool_result_to_tool_result_block_param(&data, "toolu_task_get");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert!(content.contains("Task #7: Fix auth"));
        assert!(content.contains("Blocked by: #3"));
        assert!(content.contains("Blocks: #9"));
    }
}
