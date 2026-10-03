//! Incremental port of official `TaskListTool/TaskListTool.ts`.
//! The official transcript renderer has no visible tool-use/result UI for
//! successful TaskList calls, but the model-facing result content shape is kept
//! here for tests and future readonly task-store seams.

pub mod prompt;
pub mod ui;

/// Maps to: CC `TaskListTool` metadata.
/// Maps to: CC `TaskListTool.ts:13` `inputSchema` — `z.strictObject({})`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| crate::utils::zod::strict_object(vec![]))
}

pub fn task_list_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::TASK_LIST_TOOL_NAME.to_string(),
        description: prompt::get_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/TaskListTool/TaskListTool.ts` outputSchema (:18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskListTaskOutput {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) status: String,
    pub(crate) owner: Option<String>,
    pub(crate) blocked_by: Vec<String>,
}

/// CC `tools/TaskListTool/TaskListTool.ts` outputSchema (:18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskListOutput {
    pub(crate) tasks: Vec<TaskListTaskOutput>,
}

/// List tasks from the disk TodoV2 store.
/// Maps to: CC `tools/TaskListTool/TaskListTool.ts` `call` (:65), which
/// delegates to `utils/tasks.ts` `listTasks` (:443).
pub(crate) fn task_list_output() -> TaskListOutput {
    let list_id = crate::utils::tasks::get_task_list_id();
    // CC filters `_internal` FIRST (:68-70) and collects the resolved-id set
    // from the already-filtered list (:72-75) — a completed internal task
    // therefore does NOT clear itself out of other tasks' blockedBy.
    // `!t.metadata?._internal` is a JS truthiness test, not a boolean read.
    let all_tasks = crate::utils::tasks::list_tasks(&list_id)
        .into_iter()
        .filter(|task| {
            let internal = task
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("_internal"));
            !internal.is_some_and(json_truthy)
        })
        .collect::<Vec<_>>();
    let completed_ids = all_tasks
        .iter()
        .filter(|task| task.status == "completed")
        .map(|task| task.id.clone())
        .collect::<std::collections::HashSet<_>>();
    TaskListOutput {
        tasks: all_tasks
            .into_iter()
            .map(|task| TaskListTaskOutput {
                id: task.id,
                subject: task.subject,
                status: task.status,
                owner: task.owner,
                blocked_by: task
                    .blocked_by
                    .into_iter()
                    .filter(|id| !completed_ids.contains(id))
                    .collect(),
            })
            .collect(),
    }
}

/// JS truthiness for a JSON value — `false`, `0`, `''`, and `null` are falsy,
/// everything else (objects, arrays, non-empty strings, non-zero numbers) is
/// truthy.
fn json_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(bool) => *bool,
        serde_json::Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        serde_json::Value::String(string) => !string.is_empty(),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => true,
    }
}

fn task_list_view(task: &TaskListTaskOutput) -> ui::TaskListItemView {
    ui::TaskListItemView {
        id: task.id.clone(),
        subject: task.subject.clone(),
        status: task.status.clone(),
        owner: task.owner.clone(),
        blocked_by: task.blocked_by.clone(),
    }
}

/// Behavioral half of CC `TaskListTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TaskListTool;

impl crate::tool::ToolCall for TaskListTool {
    fn name(&self) -> &'static str {
        "TaskList"
    }

    /// Maps to: CC `TaskListTool.ts:40-42` `async prompt() { return
    /// getPrompt() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `TaskListTool.ts:53-55` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        crate::utils::tasks::is_todo_v2_enabled()
    }

    /// Maps to: CC `TaskListTool.isConcurrencySafe(...)`.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `TaskListTool.ts:59-61` `isReadOnly()`.
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `TaskListTool.ts:35` `searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("list all tasks")
    }

    /// Maps to: CC `TaskListTool.ts:52` `shouldDefer: true`.
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC recording this tool's `Output` (the `call()` data) as the
    /// message's `toolUseResult` — camelCase keys, `owner` omitted when unset
    /// (CC leaves it `undefined`).
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TaskList(output) => Some(serde_json::json!({
                "tasks": output
                    .tasks
                    .iter()
                    .map(|task| {
                        let mut map = serde_json::Map::new();
                        map.insert("id".to_string(), task.id.clone().into());
                        map.insert("subject".to_string(), task.subject.clone().into());
                        map.insert("status".to_string(), task.status.clone().into());
                        if let Some(owner) = task.owner.as_ref() {
                            map.insert("owner".to_string(), owner.clone().into());
                        }
                        map.insert(
                            "blockedBy".to_string(),
                            serde_json::Value::from(task.blocked_by.clone()),
                        );
                        serde_json::Value::Object(map)
                    })
                    .collect::<Vec<_>>()
            })),
            _ => None,
        }
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
            let _ = args;
            let _ = request;
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::TaskList(task_list_output()),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/TaskListTool/TaskListTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:91-115).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::TaskList(output) => {
                let views = output.tasks.iter().map(task_list_view).collect::<Vec<_>>();
                (
                    ui::map_tool_result_content(&views),
                    crate::types::message::ToolResultStatus::Success,
                )
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>TaskList returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    // CC `TaskListTool` defines no `renderToolResultMessage` — the render
    // layer hides success rows by name (`success_tool_result_is_nonvisual`).
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn task_list_schema_matches_official_empty_input_shape() {
        let schema = super::task_list_tool_schema();
        assert_eq!(schema.name, "TaskList");
        // `z.strictObject({})` — zod emits no `required` key at all.
        assert_eq!(schema.input_schema.get("required"), None);
        assert!(schema.description.contains("list all tasks"));
    }

    #[tokio::test]
    async fn task_list_tool_call_returns_official_output_schema_and_model_copy() {
        use crate::tool::ToolCall;

        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("tool-list");
        {
            let mut tasks = crate::utils::tasks::TASK_TOOL_STORE.lock().unwrap();
            tasks.clear();
            tasks.push(crate::utils::tasks::TaskRecord {
                id: "2".to_string(),
                subject: "Run tests".to_string(),
                description: "Run regression tests".to_string(),
                active_form: None,
                status: "in_progress".to_string(),
                owner: Some("runner".to_string()),
                blocks: Vec::new(),
                blocked_by: vec!["1".to_string(), "4".to_string()],
                metadata: None,
            });
        }

        let args = serde_json::json!({});
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-task-list".to_string(),
            "toolu_task_list".to_string(),
            "TaskList".to_string(),
            "{}".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = super::TaskListTool;
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

        let crate::tool::ToolOutput::TaskList(output) = result.data else {
            panic!("TaskList should return its official ToolOutput variant");
        };
        assert_eq!(output.tasks.len(), 1);
        assert_eq!(output.tasks[0].id, "2");
        assert_eq!(output.tasks[0].owner.as_deref(), Some("runner"));

        let data = crate::tool::ToolOutput::TaskList(output);
        let (content, status) =
            tool.map_tool_result_to_tool_result_block_param(&data, "toolu_task_list");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert_eq!(
            content,
            "#2 [in_progress] Run tests (runner) [blocked by #1, #4]"
        );
    }
}
