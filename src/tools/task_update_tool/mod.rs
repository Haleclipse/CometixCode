//! Port of official `TaskUpdateTool/TaskUpdateTool.ts`.
//! The official tool uses `renderToolUseMessage() -> null`; mutations are
//! persisted through the TodoV2 disk store and failures remain model-visible.

pub mod prompt;
pub mod ui;

/// Maps to: CC `TaskUpdateTool.ts:33-66` `inputSchema`.
///
/// `status` widens the shared `TaskStatusSchema()` (`utils/tasks.rs`) with
/// `.or(z.literal('deleted'))` — a union, so it projects `anyOf`, not a single
/// four-member enum.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        let task_update_status_schema = zod::union(vec![
            crate::utils::tasks::task_status_schema().clone(),
            zod::literal(serde_json::json!("deleted")),
        ]);
        zod::strict_object(vec![
            (
                "taskId",
                zod::string().describe("The ID of the task to update"),
            ),
            (
                "subject",
                zod::string().optional().describe("New subject for the task"),
            ),
            (
                "description",
                zod::string()
                    .optional()
                    .describe("New description for the task"),
            ),
            (
                "activeForm",
                zod::string().optional().describe(
                    "Present continuous form shown in spinner when in_progress (e.g., \"Running tests\")",
                ),
            ),
            (
                "status",
                task_update_status_schema
                    .optional()
                    .describe("New status for the task"),
            ),
            (
                "addBlocks",
                zod::array(zod::string())
                    .optional()
                    .describe("Task IDs that this task blocks"),
            ),
            (
                "addBlockedBy",
                zod::array(zod::string())
                    .optional()
                    .describe("Task IDs that block this task"),
            ),
            (
                "owner",
                zod::string().optional().describe("New owner for the task"),
            ),
            (
                "metadata",
                zod::record(zod::any()).optional().describe(
                    "Metadata keys to merge into the task. Set a key to null to delete it.",
                ),
            ),
        ])
    })
}

/// Maps to: CC `TaskUpdateTool` metadata.
pub fn task_update_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::TASK_UPDATE_TOOL_NAME.to_string(),
        description: prompt::PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/TaskUpdateTool/TaskUpdateTool.ts` `statusChange` output (:75-80).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskUpdateStatusChange {
    pub(crate) from: String,
    pub(crate) to: String,
}

/// CC `tools/TaskUpdateTool/TaskUpdateTool.ts` outputSchema (:69-83).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskUpdateOutput {
    pub(crate) success: bool,
    pub(crate) task_id: String,
    pub(crate) updated_fields: Vec<String>,
    pub(crate) error: Option<String>,
    pub(crate) status_change: Option<TaskUpdateStatusChange>,
    pub(crate) verification_nudge_needed: Option<bool>,
}

/// Update a task record on disk (TodoV2).
/// Maps to: CC `tools/TaskUpdateTool/TaskUpdateTool.ts` `call` (:123), which
/// delegates to `utils/tasks.ts` `updateTask` (:370) / `blockTask` / `deleteTask`.
fn task_id_of(input: &serde_json::Value) -> String {
    // CC destructures only `taskId` — the strict schema admits no alias.
    input
        .get("taskId")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Runs TaskCompleted hooks when this update is the one that completes a task.
///
/// Maps to: CC `TaskUpdateTool.ts:231-265`. The guard is CC's exactly: the
/// requested status must be `completed` AND differ from what is stored, so
/// re-asserting an already-completed status fires nothing.
async fn run_task_completed_hooks(
    input: &serde_json::Value,
    agent_id: Option<&str>,
) -> Result<(), String> {
    // CC `TaskUpdateTool.ts:235-245` passes `context` as
    // `executeTaskCompletedHooks`' ninth argument, so `executeHooks` keys the
    // config on `toolUseContext.agentId ?? getSessionId()`
    // (`utils/hooks.ts:2003`) and `getHooksConfig` merges that session's own
    // hooks (`:1541-1563`). Same gap and same gate as TaskCreate.
    let session_id = agent_id
        .map(str::to_string)
        .unwrap_or_else(crate::bootstrap::state::get_session_id);
    let config = crate::services::hooks::load_hooks_config_with_session_hooks(&session_id);
    run_task_completed_hooks_with_config(&config, input).await
}

/// Config-injecting half, mirroring `handle_stop_hooks_with_config`.
async fn run_task_completed_hooks_with_config(
    config: &crate::services::hooks::RegisteredHooks,
    input: &serde_json::Value,
) -> Result<(), String> {
    let Some("completed") = input.get("status").and_then(|value| value.as_str()) else {
        return Ok(());
    };
    let list_id = crate::utils::tasks::get_task_list_id();
    let task_id = task_id_of(input);
    let Some(existing) = crate::utils::tasks::get_task(&list_id, &task_id) else {
        // CC reaches its hook block only after `existingTask` resolved; a
        // missing task falls through to `task_update_output`'s "Task not
        // found".
        return Ok(());
    };
    if existing.status == "completed" {
        return Ok(());
    }

    let results = crate::services::hooks::task::execute_task_completed_hooks(
        config,
        &task_id,
        &existing.subject,
        Some(existing.description.as_str()),
        crate::utils::teammate::get_agent_name().as_deref(),
        crate::utils::teammate::get_team_name(None).as_deref(),
        Vec::new(),
    )
    .await;
    let blocking: Vec<String> = results
        .iter()
        .filter_map(|result| result.blocking_error.as_ref())
        .map(crate::services::hooks::task::get_task_completed_hook_message)
        .collect();
    if blocking.is_empty() {
        Ok(())
    } else {
        Err(blocking.join("\n"))
    }
}

pub(crate) fn task_update_output(input: &serde_json::Value) -> TaskUpdateOutput {
    use crate::utils::tasks::{
        TaskUpdatePatch, block_task, delete_task, get_task, get_task_list_id, update_task,
    };

    let task_id = task_id_of(input);
    let list_id = get_task_list_id();
    let Some(existing) = get_task(&list_id, &task_id) else {
        return TaskUpdateOutput {
            success: false,
            task_id,
            updated_fields: Vec::new(),
            error: Some("Task not found".to_string()),
            status_change: None,
            verification_nudge_needed: None,
        };
    };

    let mut updated_fields = Vec::new();
    let mut patch = TaskUpdatePatch::default();
    let mut status_change = None;

    if let Some(subject) = input.get("subject").and_then(|value| value.as_str()) {
        if subject != existing.subject {
            patch.subject = Some(subject.to_string());
            updated_fields.push("subject".to_string());
        }
    }
    if let Some(description) = input.get("description").and_then(|value| value.as_str()) {
        if description != existing.description {
            patch.description = Some(description.to_string());
            updated_fields.push("description".to_string());
        }
    }
    // CC compares and assigns verbatim (:177-180) — no trimming, no
    // empty-string collapse.
    if let Some(active_form) = input.get("activeForm").and_then(|value| value.as_str()) {
        if existing.active_form.as_deref() != Some(active_form) {
            patch.active_form = Some(active_form.to_string());
            updated_fields.push("activeForm".to_string());
        }
    }
    let owner_given = input.get("owner").and_then(|value| value.as_str());
    if let Some(owner) = owner_given {
        if existing.owner.as_deref() != Some(owner) {
            patch.owner = Some(Some(owner.to_string()));
            updated_fields.push("owner".to_string());
        }
    }
    // Maps to: CC `TaskUpdateTool.ts:185-199` — when a teammate marks a task
    // in_progress without naming an owner and the task has none, the agent's
    // own name is auto-assigned so the task list can attribute activity.
    if crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
        && input.get("status").and_then(|value| value.as_str()) == Some("in_progress")
        && owner_given.is_none()
        && existing.owner.is_none()
    {
        if let Some(agent_name) = crate::utils::teammate::get_agent_name() {
            patch.owner = Some(Some(agent_name));
            updated_fields.push("owner".to_string());
        }
    }
    // CC processes metadata (:200-211) BEFORE status (:212-270) — the order
    // is observable in `updatedFields`. The zod strict schema only admits an
    // object here, so there is no null-clears-all branch.
    if let Some(meta) = input.get("metadata").and_then(|value| value.as_object()) {
        let mut merged = existing.metadata.clone().unwrap_or_default();
        for (key, value) in meta {
            if value.is_null() {
                merged.remove(key);
            } else {
                merged.insert(key.clone(), value.clone());
            }
        }
        patch.metadata = Some(Some(merged));
        updated_fields.push("metadata".to_string());
    }
    if let Some(status) = input.get("status").and_then(|value| value.as_str()) {
        if status == "deleted" {
            let from = existing.status.clone();
            if !delete_task(&list_id, &task_id) {
                return task_update_failure(
                    task_id,
                    updated_fields,
                    status_change,
                    "Failed to delete task",
                );
            }
            return TaskUpdateOutput {
                success: true,
                task_id,
                updated_fields: vec!["deleted".to_string()],
                error: None,
                status_change: Some(TaskUpdateStatusChange {
                    from,
                    to: "deleted".to_string(),
                }),
                verification_nudge_needed: None,
            };
        } else if status != existing.status {
            let from = existing.status.clone();
            patch.status = Some(status.to_string());
            updated_fields.push("status".to_string());
            status_change = Some(TaskUpdateStatusChange {
                from,
                to: status.to_string(),
            });
        }
    }

    let owner_update = patch.owner.clone().flatten();
    if patch.subject.is_some()
        || patch.description.is_some()
        || patch.active_form.is_some()
        || patch.owner.is_some()
        || patch.status.is_some()
        || patch.metadata.is_some()
    {
        if update_task(&list_id, &task_id, patch).is_none() {
            return task_update_failure(
                task_id,
                updated_fields,
                status_change,
                "Failed to persist task update",
            );
        }
    }

    // Maps to: CC `TaskUpdateTool.ts:276-298` — an ownership change notifies
    // the new owner's mailbox with a `task_assignment` message. CC passes
    // `taskListId` as the mailbox team argument; delivery is best-effort
    // there (void await), so a failed write does not fail the update.
    if let Some(new_owner) = owner_update.as_deref() {
        if crate::utils::agent_swarms_enabled::is_agent_swarms_enabled() {
            let sender_name =
                crate::utils::teammate::get_agent_name().unwrap_or_else(|| "team-lead".to_string());
            let sender_color = crate::utils::teammate::get_teammate_color();
            let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let assignment_message = serde_json::json!({
                "type": "task_assignment",
                "taskId": task_id,
                "subject": existing.subject,
                "description": existing.description,
                "assignedBy": sender_name,
                "timestamp": timestamp,
            })
            .to_string();
            let _ = crate::utils::teammate_mailbox::write_to_mailbox(
                new_owner,
                crate::utils::teammate_mailbox::TeammateMessageInput {
                    from: sender_name,
                    text: assignment_message,
                    timestamp,
                    color: sender_color,
                    summary: None,
                },
                Some(&list_id),
            );
        }
    }

    // Maps to: CC `TaskUpdateTool.ts:300-311` — only ids not already in
    // `blocks` are linked, and `updatedFields` gains 'blocks' only when at
    // least one new link was made.
    if let Some(add_blocks) = input.get("addBlocks").and_then(|value| value.as_array()) {
        let new_blocks = add_blocks
            .iter()
            .filter_map(|value| value.as_str())
            .filter(|id| !existing.blocks.iter().any(|existing_id| existing_id == id))
            .collect::<Vec<_>>();
        for block in &new_blocks {
            if !block_task(&list_id, &task_id, block) {
                return task_update_failure(
                    task_id,
                    updated_fields,
                    status_change,
                    format!("Failed to add blocked task {block}"),
                );
            }
        }
        if !new_blocks.is_empty() {
            updated_fields.push("blocks".to_string());
        }
    }
    // Maps to: CC `TaskUpdateTool.ts:313-324` — reverse link: the blocker
    // blocks this task; same already-present filter and updatedFields rule.
    if let Some(add_blocked_by) = input.get("addBlockedBy").and_then(|value| value.as_array()) {
        let new_blocked_by = add_blocked_by
            .iter()
            .filter_map(|value| value.as_str())
            .filter(|id| {
                !existing
                    .blocked_by
                    .iter()
                    .any(|existing_id| existing_id == id)
            })
            .collect::<Vec<_>>();
        for blocker in &new_blocked_by {
            if !block_task(&list_id, blocker, &task_id) {
                return task_update_failure(
                    task_id,
                    updated_fields,
                    status_change,
                    format!("Failed to add blocker task {blocker}"),
                );
            }
        }
        if !new_blocked_by.is_empty() {
            updated_fields.push("blockedBy".to_string());
        }
    }

    // CC `TaskUpdateTool.ts:326-349` computes the structural verification
    // nudge here, but only behind `feature('VERIFICATION_AGENT')` AND the
    // `tengu_hive_evidence` GrowthBook flag (both modeled off in
    // `utils/feature_flags.rs`), on the main thread (`!context.agentId`),
    // when this update set `status: completed`. With the gate off the branch
    // is unreachable — but CC initializes `let verificationNudgeNeeded =
    // false` OUTSIDE the gate (:333) and always writes it into this success
    // data (:360), so the persisted toolUseResult carries the key with
    // `false` on the non-delete success path (delete/failure paths omit it).
    TaskUpdateOutput {
        success: true,
        task_id,
        updated_fields,
        error: None,
        status_change,
        verification_nudge_needed: Some(false),
    }
}

fn task_update_failure(
    task_id: String,
    updated_fields: Vec<String>,
    status_change: Option<TaskUpdateStatusChange>,
    error: impl Into<String>,
) -> TaskUpdateOutput {
    TaskUpdateOutput {
        success: false,
        task_id,
        updated_fields,
        error: Some(error.into()),
        status_change,
        verification_nudge_needed: None,
    }
}

fn task_update_view(output: &TaskUpdateOutput) -> ui::TaskUpdateResultView {
    ui::TaskUpdateResultView {
        success: output.success,
        task_id: output.task_id.clone(),
        updated_fields: output.updated_fields.clone(),
        error: output.error.clone(),
        status_to: output
            .status_change
            .as_ref()
            .map(|change| change.to.clone()),
        // Maps to: CC `TaskUpdateTool.ts:386-394` — the "call TaskList now"
        // reminder appends when a teammate (getAgentId()) completes a task
        // under agent swarms.
        teammate_completion_nudge: crate::utils::teammate::get_agent_id().is_some()
            && crate::utils::agent_swarms_enabled::is_agent_swarms_enabled(),
        verification_nudge_needed: output.verification_nudge_needed.unwrap_or(false),
    }
}

/// Behavioral half of CC `TaskUpdateTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct TaskUpdateTool;

impl crate::tool::ToolCall for TaskUpdateTool {
    fn name(&self) -> &'static str {
        "TaskUpdate"
    }

    /// Maps to: CC `TaskUpdateTool.ts:95-97` `async prompt() { return PROMPT }`
    /// — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::PROMPT.to_string()
    }

    /// Maps to: CC `TaskUpdateTool.ts:108-110` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        crate::utils::tasks::is_todo_v2_enabled()
    }

    /// Maps to: CC `TaskUpdateTool.isConcurrencySafe(...)`.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `TaskUpdateTool.ts:90` `searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("update a task")
    }

    /// Maps to: CC `TaskUpdateTool.ts:107` `shouldDefer: true`.
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `TaskUpdateTool.ts:114-119` `toAutoClassifierInput` —
    /// taskId plus status and subject when present, space-joined.
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        let mut parts = vec![
            args.get("taskId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ];
        if let Some(status) = args.get("status").and_then(serde_json::Value::as_str) {
            parts.push(status.to_string());
        }
        if let Some(subject) = args.get("subject").and_then(serde_json::Value::as_str) {
            parts.push(subject.to_string());
        }
        parts.join(" ")
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
            // Maps to: CC `TaskUpdateTool.ts:139-143` — the tasks panel
            // auto-expands at the very top of call(), before the existence
            // check, unless it already is expanded.
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
            // Maps to: CC `TaskUpdateTool.ts:231-265` — TaskCompleted hooks run
            // BEFORE the write, and a blocking one returns `success: false`
            // with no `updatedFields` instead of persisting anything. Note the
            // contrast with TaskCreate, which writes first and rolls back:
            // there the hook needs the task id, here nothing exists to undo.
            // CC returns from the whole function, so a block also drops the
            // other field updates in the same call.
            if let Err(error) = run_task_completed_hooks(args, context.agent_id.as_deref()).await {
                return crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::TaskUpdate(TaskUpdateOutput {
                        success: false,
                        task_id: task_id_of(args),
                        updated_fields: Vec::new(),
                        error: Some(error),
                        status_change: None,
                        verification_nudge_needed: None,
                    }),
                    new_messages: Vec::new(),
                };
            }
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::TaskUpdate(task_update_output(args)),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/TaskUpdateTool/TaskUpdateTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:364-405).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            // CC returns the failure copy as a NON-error tool_result
            // (:373-381): "Return as non-error so it doesn't trigger sibling
            // tool cancellation in StreamingToolExecutor. 'Task not found' is
            // a benign condition… that the model can handle."
            crate::tool::ToolOutput::TaskUpdate(output) => (
                ui::map_tool_result_content(&task_update_view(output)),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>TaskUpdate returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording this tool's `Output` (the `call()` data) as the
    /// message's `toolUseResult` — camelCase keys; optional fields omitted
    /// when the CC construction leaves them `undefined`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::TaskUpdate(output) => {
                let mut map = serde_json::Map::new();
                map.insert("success".to_string(), output.success.into());
                map.insert("taskId".to_string(), output.task_id.clone().into());
                map.insert(
                    "updatedFields".to_string(),
                    serde_json::Value::from(output.updated_fields.clone()),
                );
                if let Some(error) = output.error.as_ref() {
                    map.insert("error".to_string(), error.clone().into());
                }
                if let Some(change) = output.status_change.as_ref() {
                    map.insert(
                        "statusChange".to_string(),
                        serde_json::json!({ "from": change.from, "to": change.to }),
                    );
                }
                if let Some(nudge) = output.verification_nudge_needed {
                    map.insert("verificationNudgeNeeded".to_string(), nudge.into());
                }
                Some(serde_json::Value::Object(map))
            }
            _ => None,
        }
    }

    // CC `TaskUpdateTool` defines no `renderToolResultMessage` — the render
    // layer hides success rows by name (`success_tool_result_is_nonvisual`);
    // failed updates keep the generic error rendering.
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn task_update_schema_matches_official_input_shape() {
        let schema = super::task_update_tool_schema();
        assert_eq!(schema.name, "TaskUpdate");
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!(["taskId"]))
        );
        // CC builds this as `TaskStatusSchema().or(z.literal('deleted'))`
        // (:35), which zod projects as a union rather than one widened enum.
        assert_eq!(
            schema.input_schema.pointer("/properties/status/anyOf"),
            Some(&serde_json::json!([
                {"type": "string", "enum": ["pending", "in_progress", "completed"]},
                {"type": "string", "const": "deleted"}
            ]))
        );
    }

    #[tokio::test]
    async fn task_update_tool_call_returns_official_output_schema_and_model_copy() {
        use crate::tool::ToolCall;

        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("tool-list");
        {
            let mut tasks = crate::utils::tasks::TASK_TOOL_STORE.lock().unwrap();
            tasks.clear();
            tasks.push(crate::utils::tasks::TaskRecord {
                id: "4".to_string(),
                subject: "Run tests".to_string(),
                description: "Run regression tests".to_string(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: None,
            });
        }

        let args = serde_json::json!({
            "taskId": "4",
            "status": "completed",
            "owner": "runner"
        });
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-task-update".to_string(),
            "toolu_task_update".to_string(),
            "TaskUpdate".to_string(),
            "4 completed".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = super::TaskUpdateTool;
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

        let crate::tool::ToolOutput::TaskUpdate(output) = result.data else {
            panic!("TaskUpdate should return its official ToolOutput variant");
        };
        assert!(output.success);
        assert_eq!(output.task_id, "4");
        assert_eq!(
            output.updated_fields,
            vec!["owner".to_string(), "status".to_string()]
        );
        assert!(matches!(
            output.status_change.as_ref(),
            Some(change) if change.from == "pending" && change.to == "completed"
        ));

        let data = crate::tool::ToolOutput::TaskUpdate(output);
        let (content, status) =
            tool.map_tool_result_to_tool_result_block_param(&data, "toolu_task_update");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert_eq!(content, "Updated task #4 owner, status");
    }

    fn blocking_task_completed_config() -> crate::services::hooks::RegisteredHooks {
        let config: crate::services::hooks::HooksConfig = serde_json::from_value(serde_json::json!({
            "TaskCompleted": [{
                "matcher": "*",
                "hooks": [{
                    "command": "printf '%s' '{\"decision\":\"block\",\"reason\":\"verify first\"}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        crate::services::hooks::test_support::registered_config(&config)
    }

    fn seed_task(status: &str) {
        let mut tasks = crate::utils::tasks::TASK_TOOL_STORE.lock().unwrap();
        tasks.clear();
        tasks.push(crate::utils::tasks::TaskRecord {
            id: "4".to_string(),
            subject: "Run tests".to_string(),
            description: "Run regression tests".to_string(),
            active_form: None,
            status: status.to_string(),
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: None,
        });
    }

    /// Maps to: CC `TaskUpdateTool.ts:231-265` — completing a task runs
    /// TaskCompleted hooks, and a blocking one stops the write.
    #[tokio::test]
    async fn blocking_task_completed_hook_reports_official_feedback() {
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("update-blocked");
        seed_task("pending");

        let blocked = super::run_task_completed_hooks_with_config(
            &blocking_task_completed_config(),
            &serde_json::json!({"taskId": "4", "status": "completed"}),
        )
        .await;

        assert_eq!(
            blocked,
            Err("TaskCompleted hook feedback:\nverify first".to_string())
        );
    }

    /// Maps to: CC `TaskUpdateTool.ts:235-245` — the ninth argument to
    /// `executeTaskCompletedHooks` is `context`, so `executeHooks` keys the
    /// config on `toolUseContext.agentId ?? getSessionId()`
    /// (`utils/hooks.ts:2003`) and `getHooksConfig` merges that session's own
    /// hooks (`:1541-1563`).
    ///
    /// Old shape: `load_hooks_config()` alone, which owns settings + registered
    /// hooks only — a TaskCompleted hook registered at runtime by an agent's
    /// frontmatter or a skill never ran, so `run_task_completed_hooks` returned
    /// `Ok(())` and the task completed unblocked. A silent pass, not a hang.
    #[tokio::test]
    async fn session_registered_task_completed_hook_runs_for_the_calling_agent() {
        use crate::services::hooks::{HookCommand, HookEvent};
        use crate::utils::hooks::session_hooks;

        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let _temp = crate::utils::tasks::TempTaskConfig::new("update-session-hook");
        seed_task("pending");

        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook(
            "agent-task-update",
            HookEvent::TaskCompleted,
            "",
            HookCommand {
                command: r#"printf '%s' '{"decision":"block","reason":"verify first"}'"#
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

        let blocked = super::run_task_completed_hooks(
            &serde_json::json!({"taskId": "4", "status": "completed"}),
            Some("agent-task-update"),
        )
        .await;
        let other_agent = super::run_task_completed_hooks(
            &serde_json::json!({"taskId": "4", "status": "completed"}),
            Some("some-other-agent"),
        )
        .await;
        session_hooks::clear_all_session_hooks();

        assert_eq!(
            blocked,
            Err("TaskCompleted hook feedback:\nverify first".to_string())
        );
        assert!(
            other_agent.is_ok(),
            "CC scopes session hooks by id (hooks.ts:1542); another agent must not inherit them"
        );
    }

    /// CC's guard is `status !== existingTask.status` — re-asserting a status
    /// the task already has fires nothing, so a blocking hook cannot wedge an
    /// already-completed task.
    #[tokio::test]
    async fn task_completed_hooks_skip_when_the_task_is_already_completed() {
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("update-noop");
        seed_task("completed");

        assert!(
            super::run_task_completed_hooks_with_config(
                &blocking_task_completed_config(),
                &serde_json::json!({"taskId": "4", "status": "completed"}),
            )
            .await
            .is_ok()
        );
    }

    /// The hooks are `TaskCompleted`, not `TaskChanged`: any other target
    /// status leaves them unfired even with a blocking hook configured.
    #[tokio::test]
    async fn task_completed_hooks_skip_for_other_status_transitions() {
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("update-other");
        seed_task("pending");

        assert!(
            super::run_task_completed_hooks_with_config(
                &blocking_task_completed_config(),
                &serde_json::json!({"taskId": "4", "status": "in_progress"}),
            )
            .await
            .is_ok()
        );
    }

    /// Maps to: CC `TaskUpdateTool.ts:185-199` (auto-owner) and :276-298
    /// (mailbox notification on ownership change) — both gated on agent
    /// swarms being enabled.
    #[tokio::test]
    async fn swarms_auto_owner_and_mailbox_notification_match_official() {
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("update-swarm");
        crate::utils::process_env::set("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "agent-7".to_string(),
                agent_name: "scout".to_string(),
                team_name: "update-swarm".to_string(),
                color: None,
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));
        seed_task("pending");

        // in_progress with no explicit owner → the agent claims it (:185-199).
        let output =
            super::task_update_output(&serde_json::json!({"taskId": "4", "status": "in_progress"}));
        assert!(output.success);
        assert!(output.updated_fields.iter().any(|field| field == "owner"));
        // The ownership change lands a task_assignment in the new owner's
        // mailbox (:276-298), addressed with the task-list id as team.
        let list_id = crate::utils::tasks::get_task_list_id();
        let inbox = crate::utils::teammate_mailbox::read_unread_messages("scout", Some(&list_id));
        assert_eq!(inbox.len(), 1, "expected one task_assignment message");
        assert!(inbox[0].text.contains("\"type\":\"task_assignment\""));
        assert_eq!(inbox[0].from, "scout");

        crate::utils::teammate::clear_dynamic_team_context();
        crate::utils::process_env::remove("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
    }

    /// Maps to: CC `TaskUpdateTool.ts:300-324` — already-linked ids are
    /// filtered before blockTask and `updatedFields` only records the field
    /// when at least one NEW link was made.
    #[tokio::test]
    async fn add_blocks_skips_already_present_links_like_official() {
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("update-blocks");
        {
            let mut tasks = crate::utils::tasks::TASK_TOOL_STORE.lock().unwrap();
            tasks.clear();
            tasks.push(crate::utils::tasks::TaskRecord {
                id: "4".to_string(),
                subject: "Run tests".to_string(),
                description: "Run regression tests".to_string(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: vec!["9".to_string()],
                blocked_by: Vec::new(),
                metadata: None,
            });
        }

        let output =
            super::task_update_output(&serde_json::json!({"taskId": "4", "addBlocks": ["9"]}));
        assert!(output.success);
        assert!(
            output.updated_fields.is_empty(),
            "an already-present link must not record 'blocks': {:?}",
            output.updated_fields
        );
    }

    /// Maps to: CC `TaskUpdateTool.ts:373-381` — the failure copy returns as
    /// a NON-error tool_result so StreamingToolExecutor does not cancel
    /// sibling tools over a benign "Task not found".
    #[tokio::test]
    async fn failed_update_maps_to_non_error_tool_result() {
        use crate::tool::ToolCall;
        let data = crate::tool::ToolOutput::TaskUpdate(super::TaskUpdateOutput {
            success: false,
            task_id: "404".to_string(),
            updated_fields: Vec::new(),
            error: Some("Task not found".to_string()),
            status_change: None,
            verification_nudge_needed: None,
        });
        let (content, status) = super::TaskUpdateTool
            .map_tool_result_to_tool_result_block_param(&data, "toolu_task_update");
        assert_eq!(content, "Task not found");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
    }
}
