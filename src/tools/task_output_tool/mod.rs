//! TaskOutput tool metadata and UI.
//!
//! Maps to:
//! - CC `tools/TaskOutputTool/TaskOutputTool.tsx`
//! - CC `tools/TaskOutputTool/constants.ts`
//! - CC `tools/TaskOutputTool/UI.tsx`
//!
//! Execution lives in this module, dispatched from `services/tools/tool_execution.rs`.

pub mod constants;
pub mod ui;

/// Maps to: CC `TaskOutputTool.tsx:30-43` `inputSchema`.
///
/// `block` and `timeout` carry a `.default()` with no outer `.optional()`, so
/// zod keeps both in `required` while still emitting the defaults. `timeout`'s
/// bounds are `.min(0).max(600000)` — inclusive, hence `minimum`/`maximum`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "task_id",
                zod::string().describe("The task ID to get output from"),
            ),
            (
                "block",
                crate::utils::semantic_boolean::semantic_boolean(
                    zod::boolean().default(serde_json::json!(true)),
                )
                .describe("Whether to wait for completion"),
            ),
            (
                "timeout",
                zod::number()
                    .min(0)
                    .max(600_000)
                    .default(serde_json::json!(30000))
                    .describe("Max wait time in ms"),
            ),
        ])
    })
}

/// Maps to: CC `TaskOutputTool.tsx:205-215` `async prompt()` literal — the
/// single owner shared by the wire schema and `ToolCall::prompt`.
pub(crate) const API_PROMPT: &str = "DEPRECATED: Prefer using the Read tool on the task's output file path instead. Background tasks return their output file path in the tool result, and you receive a <task-notification> with the same path when the task completes — Read that file directly.\n\n- Retrieves output from a running or completed task (background shell, agent, or remote session)\n- Takes a task_id parameter identifying the task\n- Returns the task output along with status information\n- Use block=true (default) to wait for task completion\n- Use block=false for non-blocking check of current status\n- Task IDs can be found using the /tasks command\n- Works with all task types: background shells, async agents, and remote sessions";

/// Maps to CC `TaskOutputTool.inputSchema`.
pub fn task_output_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: constants::TASK_OUTPUT_TOOL_NAME.to_string(),
        // Maps to: CC `TaskOutputTool.aliases` — AgentOutputTool / BashOutputTool.
        aliases: vec!["AgentOutputTool".to_string(), "BashOutputTool".to_string()],
        description: API_PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/TaskOutputTool/TaskOutputTool.tsx` `TaskOutput` (:49).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskOutput {
    pub(crate) task_id: String,
    pub(crate) task_type: String,
    pub(crate) status: String,
    pub(crate) description: String,
    pub(crate) output: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) error: Option<String>,
    pub(crate) prompt: Option<String>,
    pub(crate) result: Option<String>,
}

/// CC `tools/TaskOutputTool/TaskOutputTool.tsx` `TaskOutputToolOutput` (:62).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) retrieval_status: String,
    pub(crate) task: Option<TaskOutput>,
}

fn task_identity_snapshot(
    task_id: &str,
    app_store: Option<&crate::state::store::AppStore>,
) -> Option<crate::tasks::stop_task::TaskLookupSnapshot> {
    crate::tasks::stop_task::lookup_task(task_id, app_store)
}

fn task_output_snapshot(
    task_id: &str,
    app_store: Option<&crate::state::store::AppStore>,
) -> Option<TaskOutput> {
    if let Some(task) = crate::tasks::local_shell_task::task_output_snapshot(task_id, app_store) {
        return Some(TaskOutput {
            task_id: task.id,
            task_type: "local_bash".to_string(),
            status: task.status,
            description: task.description,
            output: task.output,
            exit_code: task.exit_code,
            error: None,
            prompt: None,
            result: None,
        });
    }
    if let Some(task) = crate::tasks::local_agent_task::task_output_snapshot(task_id) {
        return Some(TaskOutput {
            task_id: task.task_id,
            task_type: task.task_type,
            status: task.status,
            description: task.description,
            output: task.output,
            exit_code: task.exit_code,
            error: task.error,
            prompt: task.prompt,
            result: task.result,
        });
    }
    if let Some(task) =
        crate::tasks::in_process_teammate_task::get_in_process_teammate_task(task_id)
    {
        return Some(TaskOutput {
            task_id: task.task_id.clone(),
            task_type: task.task_type,
            status: task.status,
            description: task.description,
            output: crate::utils::task::disk_output::get_task_output(
                &task.task_id,
                8 * 1024 * 1024,
            ),
            exit_code: None,
            error: task.error,
            prompt: None,
            result: None,
        });
    }
    let state = app_store?.get();
    let (task_id, task_type, status, description) = match state.tasks.get(task_id)?.as_ref() {
        crate::state::app_state_store::TaskState::InProcessTeammate(task) => (
            task.id.clone(),
            task.task_type.clone(),
            task.status.clone(),
            task.agent_name.clone(),
        ),
        crate::state::app_state_store::TaskState::LocalShell(task) => (
            task.id.clone(),
            task.task_type.clone(),
            task.status.clone(),
            task.description.clone(),
        ),
        crate::state::app_state_store::TaskState::Dream(task) => (
            task.id.clone(),
            task.task_type.clone(),
            task.status.clone(),
            task.description.clone(),
        ),
        crate::state::app_state_store::TaskState::Other(task) => (
            task.id.clone(),
            task.task_type.clone(),
            task.status.clone(),
            task.description.clone(),
        ),
    };
    Some(TaskOutput {
        output: crate::utils::task::disk_output::get_task_output(&task_id, 8 * 1024 * 1024),
        task_id,
        task_type,
        status,
        description,
        exit_code: None,
        error: None,
        prompt: None,
        result: None,
    })
}

fn is_pending_or_running(status: &str) -> bool {
    matches!(status, "pending" | "running")
}

fn mark_task_notified(task_id: &str) {
    if crate::tasks::local_shell_task::task_identity_snapshot(task_id, None).is_some() {
        crate::tasks::local_shell_task::mark_task_notified(task_id);
    } else if crate::tasks::local_agent_task::task_identity_snapshot(task_id).is_some() {
        crate::tasks::local_agent_task::mark_agent_task_notified(task_id);
    } else {
        crate::tasks::in_process_teammate_task::mark_in_process_teammate_notified(task_id);
    }
}

/// Query a task's output, optionally waiting for a terminal state.
///
/// Maps to CC `tools/TaskOutputTool/TaskOutputTool.tsx:71-164,240-330`.
/// CC's abortable async `sleep(100)` polling loop maps directly to Tokio sleep;
/// no retained/render thread is blocked. At the Rust `ToolCall -> ToolResult`
/// boundary, a surviving abort is represented as an error ToolResult string;
/// the query actor normally consumes turn aborts before this fallback escapes.
pub(crate) async fn task_output_output(
    input: &serde_json::Value,
    app_store: Option<&crate::state::store::AppStore>,
    abort_controller: &crate::tool::AbortController,
    progress: Option<crate::tool::ToolCallProgressFn<'_>>,
    tool_use_id: Option<&str>,
) -> Result<Output, String> {
    let Some(task_id) = input.get("task_id").and_then(|value| value.as_str()) else {
        return Err("Task ID is required".to_string());
    };
    let block = input
        .get("block")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let timeout_ms = input
        .get("timeout")
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(30_000.0)
        .clamp(0.0, 600_000.0);
    let timeout = std::time::Duration::from_secs_f64(timeout_ms / 1000.0);

    let Some(initial_task) = task_identity_snapshot(task_id, app_store) else {
        return Err(format!("No task found with ID: {task_id}"));
    };

    if !block {
        let terminal = !is_pending_or_running(&initial_task.status);
        if terminal {
            mark_task_notified(task_id);
        }
        return Ok(Output {
            retrieval_status: if terminal { "success" } else { "not_ready" }.to_string(),
            task: task_output_snapshot(task_id, app_store),
        });
    }

    if let (Some(progress), Some(tool_use_id)) = (progress, tool_use_id) {
        // CC emits a synthetic progress-message ID; Rust progress transport
        // uses the parent tool-use ID so REPL can update the retained row.
        progress(crate::types::tools::ToolProgress::TaskOutputWaiting {
            tool_use_id: crate::types::ids::ToolUseId(tool_use_id.to_string()),
            task_description: initial_task.description,
            task_type: initial_task.task_type,
        });
    }

    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if abort_controller.is_aborted() {
            return Err("TaskOutput was aborted".to_string());
        }
        let Some(task) = task_identity_snapshot(task_id, app_store) else {
            return Ok(Output {
                retrieval_status: "timeout".to_string(),
                task: None,
            });
        };
        if !is_pending_or_running(&task.status) {
            mark_task_notified(task_id);
            return Ok(Output {
                retrieval_status: "success".to_string(),
                task: task_output_snapshot(task_id, app_store),
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let Some(task) = task_identity_snapshot(task_id, app_store) else {
        return Ok(Output {
            retrieval_status: "timeout".to_string(),
            task: None,
        });
    };
    if is_pending_or_running(&task.status) {
        Ok(Output {
            retrieval_status: "timeout".to_string(),
            task: task_output_snapshot(task_id, app_store),
        })
    } else {
        mark_task_notified(task_id);
        Ok(Output {
            retrieval_status: "success".to_string(),
            task: task_output_snapshot(task_id, app_store),
        })
    }
}

/// Official `<retrieval_status>`/`<task_id>` model content assembly.
/// Maps to: CC `tools/TaskOutputTool/TaskOutputTool.tsx`
/// `mapToolResultToToolResultBlockParam` (:333-363).
pub(crate) fn task_output_model_content(output: &Output) -> String {
    let mut parts = vec![format!(
        "<retrieval_status>{}</retrieval_status>",
        output.retrieval_status
    )];
    let Some(task) = &output.task else {
        return parts.join("\n\n");
    };
    parts.push(format!("<task_id>{}</task_id>", task.task_id));
    parts.push(format!("<task_type>{}</task_type>", task.task_type));
    parts.push(format!("<status>{}</status>", task.status));
    if let Some(exit_code) = task.exit_code {
        parts.push(format!("<exit_code>{exit_code}</exit_code>"));
    }
    if !task.output.trim().is_empty() {
        let formatted =
            crate::utils::task::output_formatting::format_task_output(&task.output, &task.task_id);
        parts.push(format!(
            "<output>\n{}\n</output>",
            formatted.content.trim_end()
        ));
    }
    if let Some(error) = &task.error {
        parts.push(format!("<error>{error}</error>"));
    }
    parts.join("\n\n")
}

fn task_output_error_result(error: String) -> crate::tool::ToolResult {
    crate::tool::ToolResult {
        data: crate::tool::ToolOutput::Composed {
            content: error,
            status: crate::types::message::ToolResultStatus::Error,
        },
        new_messages: Vec::new(),
    }
}

/// Behavioral half of CC `TaskOutputTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TaskOutputTool;

impl crate::tool::ToolCall for TaskOutputTool {
    fn name(&self) -> &'static str {
        "TaskOutput"
    }

    /// Maps to: CC `TaskOutputTool.tsx:205-215` `async prompt()` — the
    /// deprecation-note literal, shared with the wire schema via [`API_PROMPT`].
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        API_PROMPT.to_string()
    }

    /// Maps to: CC `TaskOutputTool.tsx:178-180` `userFacingName() => 'Task
    /// Output'` (with a space, unlike the tool name).
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "Task Output".to_string()
    }

    /// Maps to: CC `TaskOutputTool.aliases`.
    fn aliases(&self) -> &'static [&'static str] {
        &["AgentOutputTool", "BashOutputTool"]
    }

    /// Maps to: CC `TaskOutputTool.tsx:30-42`. Zod converts exact quoted
    /// booleans and materializes the `block`/`timeout` defaults before every
    /// downstream consumer observes the input.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut parsed = args.clone();
        crate::utils::semantic_boolean::preprocess_object_field(&mut parsed, "block");
        if let Some(object) = parsed.as_object_mut() {
            object
                .entry("block".to_string())
                .or_insert(serde_json::Value::Bool(true));
            object
                .entry("timeout".to_string())
                .or_insert(serde_json::json!(30_000));
        }
        parsed
    }

    /// Maps to: CC `TaskOutputTool.isConcurrencySafe(_input)`
    /// (TaskOutputTool.tsx:190) delegating to `isReadOnly` (:198) — true.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("read output/logs from a background task")
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("task_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        let Some(task_id) = args.get("task_id").and_then(serde_json::Value::as_str) else {
            return crate::tool::ValidationResult::error("Task ID is required", 1);
        };
        let app_store = context
            .app_store
            .tasks_store
            .as_ref()
            .or(context.app_store.store.as_ref());
        if crate::tasks::stop_task::lookup_task(task_id, app_store).is_none() {
            return crate::tool::ValidationResult::error(
                format!("No task found with ID: {task_id}"),
                2,
            );
        }
        crate::tool::ValidationResult::Ok
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let app_store = context
                .app_store
                .tasks_store
                .clone()
                .or_else(|| context.app_store.store.clone());
            match task_output_output(
                args,
                app_store.as_ref(),
                &context.abort_controller,
                on_progress,
                Some(&request.tool_use_id),
            )
            .await
            {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::TaskOutput(output),
                    new_messages: Vec::new(),
                },
                Err(error) => task_output_error_result(error),
            }
        })
    }

    /// Maps to: CC `tools/TaskOutputTool/TaskOutputTool.tsx`
    /// `mapToolResultToToolResultBlockParam` (:333-363).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::TaskOutput(output) => (
                task_output_model_content(output),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>TaskOutput returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording TaskOutputTool's `TaskOutputToolOutput` as the
    /// message's `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TaskOutput(output) => Some(ui::output_to_value(output)),
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
    fn task_output_tool_schema_matches_official_input_shape() {
        let schema = task_output_tool_schema();
        assert_eq!(schema.name, "TaskOutput");
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["task_id", "block", "timeout"])
        );
        assert_eq!(schema.input_schema["properties"]["block"]["default"], true);
        assert_eq!(
            schema.input_schema["properties"]["timeout"]["default"],
            30000
        );
        assert_eq!(
            schema.input_schema["properties"]["timeout"]["maximum"],
            600000
        );
    }

    #[test]
    fn task_output_semantic_boolean_and_defaults_match_official_schema_parse() {
        let parsed = crate::tool::ToolCall::normalize_input(
            &TaskOutputTool,
            &serde_json::json!({"task_id": "task-1", "block": "false"}),
        );
        assert_eq!(parsed["block"], serde_json::json!(false));
        assert_eq!(parsed["timeout"], serde_json::json!(30_000));

        let defaults = crate::tool::ToolCall::normalize_input(
            &TaskOutputTool,
            &serde_json::json!({"task_id": "task-1"}),
        );
        assert_eq!(defaults["block"], serde_json::json!(true));
        assert_eq!(defaults["timeout"], serde_json::json!(30_000));
    }

    #[tokio::test]
    async fn task_output_reads_local_agent_task_snapshot_like_official() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
        );
        crate::tasks::local_agent_task::complete_agent_task(
            &crate::tools::agent_tool::agent_tool_utils::CompletedAgentRun {
                agent_id: task_id.clone(),
                agent_type: "general-purpose".to_string(),
                content: vec!["done".to_string()],
                messages: Vec::new(),
                total_tool_use_count: 0,
                total_duration_ms: 1,
                total_tokens: 2,
                usage: None,
                content_replacement_state: None,
            },
        );

        let output = task_output_output(
            &serde_json::json!({"task_id": task_id}),
            None,
            &crate::tool::AbortController::default(),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.retrieval_status, "success");
        let task = output.task.unwrap();
        assert_eq!(task.task_type, "local_agent");
        assert_eq!(task.status, "completed");
        assert_eq!(task.result.as_deref(), Some("done"));
        crate::utils::message_queue_manager::clear_command_queue();
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task.task_id);
    }

    #[tokio::test]
    async fn task_output_wait_is_abortable_emits_progress_and_marks_terminal_retrieval() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
                    "general-purpose",
                    "Use for general tasks",
                    crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
                ),
                tool_use_id: None,
            },
        );

        let progress_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let progress_events_for_callback = progress_events.clone();
        let progress = move |event| progress_events_for_callback.lock().unwrap().push(event);
        let abort = crate::tool::AbortController::default();
        abort.abort();
        let error = task_output_output(
            &serde_json::json!({"task_id": task_id, "timeout": 1000}),
            None,
            &abort,
            Some(&progress),
            Some("toolu_task_output"),
        )
        .await
        .unwrap_err();
        assert_eq!(error, "TaskOutput was aborted");
        assert!(matches!(
            progress_events.lock().unwrap().as_slice(),
            [crate::types::tools::ToolProgress::TaskOutputWaiting {
                tool_use_id,
                task_description,
                task_type,
            }] if tool_use_id.0 == "toolu_task_output"
                && task_description == "inspect"
                && task_type == "local_agent"
        ));

        assert!(crate::tasks::local_agent_task::kill_async_agent(&task_id));
        assert!(
            !crate::tasks::local_agent_task::get_local_agent_task(&task_id)
                .unwrap()
                .notified
        );
        let output = task_output_output(
            &serde_json::json!({"task_id": task_id, "block": false}),
            None,
            &crate::tool::AbortController::default(),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.retrieval_status, "success");
        assert_eq!(output.task.unwrap().status, "killed");
        assert!(
            crate::tasks::local_agent_task::get_local_agent_task(&task_id)
                .unwrap()
                .notified
        );

        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
    }

    #[tokio::test]
    async fn task_output_polling_yields_to_runtime_until_task_is_terminal() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
                    "general-purpose",
                    "Use for general tasks",
                    crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
                ),
                tool_use_id: None,
            },
        );

        let task_id_for_kill = task_id.clone();
        let args = serde_json::json!({"task_id": task_id.clone(), "timeout": 1000});
        let abort = crate::tool::AbortController::default();
        let (output, killed) = tokio::join!(
            task_output_output(&args, None, &abort, None, None),
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                crate::tasks::local_agent_task::kill_async_agent(&task_id_for_kill)
            }
        );
        assert!(killed, "the sibling future must run while TaskOutput waits");
        let output = output.unwrap();
        assert_eq!(output.retrieval_status, "success");
        assert_eq!(output.task.unwrap().status, "killed");

        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
    }

    #[test]
    fn task_output_behavior_metadata_matches_official_consumers() {
        let tool = TaskOutputTool;
        assert!(crate::tool::ToolCall::is_enabled(&tool));
        assert_eq!(
            crate::tool::ToolCall::search_hint(&tool),
            Some("read output/logs from a background task")
        );
        assert!(crate::tool::ToolCall::should_defer(&tool));
        assert_eq!(crate::tool::ToolCall::max_result_size_chars(&tool), 100_000);
        assert_eq!(
            crate::tool::ToolCall::to_auto_classifier_input(
                &tool,
                &serde_json::json!({"task_id":"task-1"}),
            ),
            "task-1"
        );
        assert_eq!(
            crate::tool::ToolCall::validate_input(
                &tool,
                &serde_json::json!({"task_id":"missing-task"}),
                &crate::tool::ToolUseContext::default(),
            ),
            crate::tool::ValidationResult::error("No task found with ID: missing-task", 2,)
        );
    }
}
