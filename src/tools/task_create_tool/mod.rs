//! Incremental port of official `TaskCreateTool/TaskCreateTool.ts`.
//! The official tool has a visible `userFacingName`, but returns `null` from
//! `renderToolUseMessage()`, so `AssistantToolUseMessage` renders no row.
//! Success results also have no `renderToolResultMessage()` and remain hidden.

pub mod prompt;
pub mod ui;

/// Maps to: CC `TaskCreateTool.ts:18-33` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "subject",
                zod::string().describe("A brief title for the task"),
            ),
            (
                "description",
                zod::string().describe("What needs to be done"),
            ),
            (
                "activeForm",
                zod::string().optional().describe(
                    "Present continuous form shown in spinner when in_progress (e.g., \"Running tests\")",
                ),
            ),
            (
                "metadata",
                zod::record(zod::any())
                    .optional()
                    .describe("Arbitrary metadata to attach to the task"),
            ),
        ])
    })
}

/// Maps to: CC `TaskCreateTool` metadata.
pub fn task_create_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::TASK_CREATE_TOOL_NAME.to_string(),
        description: prompt::get_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/TaskCreateTool/TaskCreateTool.ts` outputSchema (:36-43).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskCreateTaskOutput {
    pub(crate) id: String,
    pub(crate) subject: String,
}

/// CC `tools/TaskCreateTool/TaskCreateTool.ts` outputSchema (:36-43).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskCreateOutput {
    pub(crate) task: TaskCreateTaskOutput,
}

/// Create a task record on disk under `getTasksDir`.
/// Maps to: CC `tools/TaskCreateTool/TaskCreateTool.ts` `call` (:80), which
/// delegates persistence to `utils/tasks.ts` `createTask` (:284).
pub(crate) fn task_create_output(input: &serde_json::Value) -> Result<TaskCreateOutput, String> {
    use crate::utils::tasks::{NewTaskData, create_task, get_task_list_id};

    // CC destructures the schema-validated fields and passes them through
    // verbatim (:80-90) — no fallback title, no activeForm trimming.
    let subject = input
        .get("subject")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let description = input
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let active_form = input
        .get("activeForm")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let metadata = input
        .get("metadata")
        .and_then(|value| value.as_object())
        .cloned();

    let list_id = get_task_list_id();
    let id = create_task(
        &list_id,
        NewTaskData {
            subject: subject.clone(),
            description,
            active_form,
            status: "pending".to_string(),
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata,
        },
    )
    .map_err(|err| format!("Failed to create task: {err}"))?;
    Ok(TaskCreateOutput {
        task: TaskCreateTaskOutput { id, subject },
    })
}

/// Runs the TaskCreated hooks for a freshly persisted task.
///
/// Maps to: CC `TaskCreateTool.ts:92-113`. `Err` carries the joined blocking
/// feedback, which is the caller's signal to delete the task and fail the
/// tool call — CC throws it, so the model sees why its task was rejected.
async fn run_task_created_hooks(
    output: &TaskCreateOutput,
    description: Option<&str>,
    agent_id: Option<&str>,
) -> Result<(), String> {
    // CC `TaskCreateTool.ts:93-103` passes `context` as
    // `executeTaskCreatedHooks`' ninth argument, which reaches
    // `executeHooks({toolUseContext})` and therefore
    // `getHooksConfig(appState, toolUseContext.agentId ?? getSessionId(), …)`
    // (`utils/hooks.ts:2001-2010`, `:1541-1563`). `load_hooks_config()` covers
    // only settings + registered hooks, so a session-registered TaskCreated
    // hook (agent frontmatter / skill) never ran. Gate mirrors CC `:1516`.
    let session_id = agent_id
        .map(str::to_string)
        .unwrap_or_else(crate::bootstrap::state::get_session_id);
    let config = crate::services::hooks::load_hooks_config_with_session_hooks(&session_id);
    run_task_created_hooks_with_config(&config, output, description).await
}

/// Config-injecting half, mirroring `handle_stop_hooks_with_config`'s split:
/// the production entry reads the merged settings, tests hand in a config.
async fn run_task_created_hooks_with_config(
    config: &crate::services::hooks::RegisteredHooks,
    output: &TaskCreateOutput,
    description: Option<&str>,
) -> Result<(), String> {
    let results = crate::services::hooks::task::execute_task_created_hooks(
        config,
        &output.task.id,
        &output.task.subject,
        // CC passes the tool's `description` argument (:96).
        description,
        crate::utils::teammate::get_agent_name().as_deref(),
        crate::utils::teammate::get_team_name(None).as_deref(),
        Vec::new(),
    )
    .await;
    let blocking: Vec<String> = results
        .iter()
        .filter_map(|result| result.blocking_error.as_ref())
        .map(crate::services::hooks::task::get_task_created_hook_message)
        .collect();
    if blocking.is_empty() {
        Ok(())
    } else {
        Err(blocking.join("\n"))
    }
}

/// Behavioral half of CC `TaskCreateTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TaskCreateTool;

impl crate::tool::ToolCall for TaskCreateTool {
    fn name(&self) -> &'static str {
        "TaskCreate"
    }

    /// Maps to: CC `TaskCreateTool.ts:55-57` `async prompt() { return
    /// getPrompt() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `TaskCreateTool.ts:68-70` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        crate::utils::tasks::is_todo_v2_enabled()
    }

    /// Maps to: CC `TaskCreateTool.isConcurrencySafe(...)`.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `TaskCreateTool.ts:50` `searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("create a task in the task list")
    }

    /// Maps to: CC `TaskCreateTool.ts:67` `shouldDefer: true`.
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `TaskCreateTool.ts:74-76` `toAutoClassifierInput` —
    /// `input.subject`.
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("subject")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let _ = request;
            let error_result = |error: String| crate::tool::ToolResult {
                data: crate::tool::ToolOutput::Composed {
                    content: error,
                    status: crate::types::message::ToolResultStatus::Error,
                },
                new_messages: Vec::new(),
            };
            let output = match task_create_output(args) {
                Ok(output) => output,
                Err(error) => return error_result(error),
            };
            // Maps to: CC `TaskCreateTool.ts:92-113` — TaskCreated hooks run
            // AFTER the task is persisted, and a blocking one un-does it:
            // `await deleteTask(getTaskListId(), taskId)` then throw. Creating
            // first is what lets a hook see the real task id.
            let description = args.get("description").and_then(|value| value.as_str());
            if let Err(feedback) =
                run_task_created_hooks(&output, description, context.agent_id.as_deref()).await
            {
                crate::utils::tasks::delete_task(
                    &crate::utils::tasks::get_task_list_id(),
                    &output.task.id,
                );
                return error_result(feedback);
            }
            // Maps to: CC `TaskCreateTool.ts:115-119` — creating a task
            // auto-expands the tasks panel unless it already is expanded
            // (`if (prev.expandedView === 'tasks') return prev`).
            if let Some(store) = context
                .app_store
                .tasks_store
                .as_ref()
                .or(context.app_store.store.as_ref())
            {
                if store.get().expanded_view != crate::state::app_state_store::ExpandedView::Tasks {
                    store.replace_with(|state| {
                        state.expanded_view = crate::state::app_state_store::ExpandedView::Tasks;
                    });
                }
            }
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::TaskCreate(output),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/TaskCreateTool/TaskCreateTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:130-137).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::TaskCreate(output) => (
                ui::map_tool_result_content(&output.task.id, &output.task.subject),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>TaskCreate returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording this tool's `Output` (the `call()` data) as the
    /// message's `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TaskCreate(output) => Some(serde_json::json!({
                "task": { "id": output.task.id, "subject": output.task.subject }
            })),
            _ => None,
        }
    }

    // CC `TaskCreateTool` defines no `renderToolResultMessage` — the render
    // layer hides success rows by name (`success_tool_result_is_nonvisual`).
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn task_create_schema_matches_official_input_shape() {
        let schema = super::task_create_tool_schema();
        assert_eq!(schema.name, "TaskCreate");
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!(["subject", "description"]))
        );
        assert!(schema.description.contains("structured task list"));
    }

    #[tokio::test]
    async fn task_create_tool_call_returns_official_output_schema_and_model_copy() {
        use crate::tool::ToolCall;

        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("create-list");

        let args =
            serde_json::json!({"subject": "Review auth", "description": "Inspect auth flow"});
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-task-create".to_string(),
            "toolu_task_create".to_string(),
            "TaskCreate".to_string(),
            "Review auth".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = super::TaskCreateTool;
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

        let crate::tool::ToolOutput::TaskCreate(output) = result.data else {
            panic!("TaskCreate should return its official ToolOutput variant");
        };
        assert_eq!(output.task.id, "1");
        assert_eq!(output.task.subject, "Review auth");

        let data = crate::tool::ToolOutput::TaskCreate(output);
        let (content, status) =
            tool.map_tool_result_to_tool_result_block_param(&data, "toolu_task_create");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert_eq!(content, "Task #1 created successfully: Review auth");
    }

    /// Maps to: CC `TaskCreateTool.ts:92-113`. A blocking TaskCreated hook
    /// yields the `TaskCreated hook feedback:` text, which the caller turns
    /// into `deleteTask(...)` plus a thrown error — so the model learns the
    /// task was rejected instead of believing it exists.
    #[tokio::test]
    async fn blocking_task_created_hook_reports_official_feedback() {
        let config: crate::services::hooks::HooksConfig = serde_json::from_value(serde_json::json!({
            "TaskCreated": [{
                "matcher": "*",
                "hooks": [{
                    "command": "printf '%s' '{\"decision\":\"block\",\"reason\":\"tasks are frozen\"}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let config = crate::services::hooks::test_support::registered_config(&config);
        let output = super::TaskCreateOutput {
            task: super::TaskCreateTaskOutput {
                id: "1".to_string(),
                subject: "Review auth".to_string(),
            },
        };

        let blocked =
            super::run_task_created_hooks_with_config(&config, &output, Some("Inspect auth flow"))
                .await;

        assert_eq!(
            blocked,
            Err("TaskCreated hook feedback:\ntasks are frozen".to_string())
        );
    }

    /// Maps to: CC `TaskCreateTool.ts:93-103` — the ninth argument to
    /// `executeTaskCreatedHooks` is `context`, so `executeHooks` keys the config
    /// on `toolUseContext.agentId ?? getSessionId()` (`utils/hooks.ts:2003`) and
    /// `getHooksConfig` merges that session's own hooks (`:1541-1563`).
    ///
    /// Old shape: `load_hooks_config()` alone, which owns settings + registered
    /// hooks only — a TaskCreated hook registered at runtime by an agent's
    /// frontmatter or a skill never ran, so `run_task_created_hooks` returned
    /// `Ok(())` and the task stood. A silent pass, not a hang.
    #[tokio::test]
    async fn session_registered_task_created_hook_runs_for_the_calling_agent() {
        use crate::services::hooks::{HookCommand, HookEvent};
        use crate::utils::hooks::session_hooks;

        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);

        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook(
            "agent-task-create",
            HookEvent::TaskCreated,
            "",
            HookCommand {
                command: r#"printf '%s' '{"decision":"block","reason":"tasks are frozen"}'"#
                    .to_string(),
                shell: None,
                timeout: Some(5),
                condition: None,
                status: None,
                once: None,
                is_async: None,
                async_rewake: None,
            },
        );
        let output = super::TaskCreateOutput {
            task: super::TaskCreateTaskOutput {
                id: "1".to_string(),
                subject: "Review auth".to_string(),
            },
        };

        let blocked = super::run_task_created_hooks(&output, None, Some("agent-task-create")).await;
        let other_agent =
            super::run_task_created_hooks(&output, None, Some("some-other-agent")).await;
        session_hooks::clear_all_session_hooks();

        assert_eq!(
            blocked,
            Err("TaskCreated hook feedback:\ntasks are frozen".to_string())
        );
        assert!(
            other_agent.is_ok(),
            "CC scopes session hooks by id (hooks.ts:1542); another agent must not inherit them"
        );
    }

    /// The same path with no TaskCreated hook configured must not block.
    #[tokio::test]
    async fn task_created_hooks_pass_when_none_are_configured() {
        let config = crate::services::hooks::RegisteredHooks::default();
        let output = super::TaskCreateOutput {
            task: super::TaskCreateTaskOutput {
                id: "1".to_string(),
                subject: "Review auth".to_string(),
            },
        };

        assert!(
            super::run_task_created_hooks_with_config(&config, &output, None)
                .await
                .is_ok()
        );
    }

    /// Maps to: CC `TaskCreateTool.ts:115-119` — a successful create
    /// auto-expands the tasks panel via setAppState.
    #[tokio::test]
    async fn create_auto_expands_the_tasks_panel_like_official() {
        use crate::tool::ToolCall;

        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("create-expand");

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut context = crate::tool::ToolUseContext::default();
        context.app_store = crate::tool::AppStoreRef::new(store.clone());
        assert_eq!(
            store.get().expanded_view,
            crate::state::app_state_store::ExpandedView::None
        );

        let args = serde_json::json!({"subject": "Review auth", "description": "Inspect"});
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-task-create-expand".to_string(),
            "toolu_task_create_expand".to_string(),
            "TaskCreate".to_string(),
            "Review auth".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let result = super::TaskCreateTool
            .call(&args, &request, &context, None, None, None)
            .await;
        assert!(matches!(
            result.data,
            crate::tool::ToolOutput::TaskCreate(_)
        ));
        assert_eq!(
            store.get().expanded_view,
            crate::state::app_state_store::ExpandedView::Tasks
        );
    }
}
