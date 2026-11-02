//! Shared task stopping logic.
//!
//! Maps to CC `tasks/stopTask.ts:10-73`.
//! TaskStopTool and the SDK `stop_task` control both delegate here so lookup,
//! status validation, kill dispatch, and shell-notification suppression have a
//! single production owner.

use crate::state::store::AppStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopTaskErrorCode {
    NotFound,
    NotRunning,
    UnsupportedType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopTaskError {
    pub code: StopTaskErrorCode,
    message: String,
}

impl StopTaskError {
    fn new(message: impl Into<String>, code: StopTaskErrorCode) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StopTaskError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StopTaskError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopTaskResult {
    pub task_id: String,
    pub task_type: String,
    pub command: Option<String>,
}

fn not_running(task_id: &str, status: &str) -> StopTaskError {
    StopTaskError::new(
        format!("Task {task_id} is not running (status: {status})"),
        StopTaskErrorCode::NotRunning,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskLookupSnapshot {
    pub status: String,
    pub task_type: String,
    pub description: String,
    pub command: Option<String>,
}

/// Unified lightweight lookup for TaskOutput validation/polling and stopTask.
/// Maps to CC reading `getAppState().tasks[taskId]` in
/// `TaskOutputTool.tsx:135-164` and `tasks/stopTask.ts:43-53`.
pub fn lookup_task(task_id: &str, app_store: Option<&AppStore>) -> Option<TaskLookupSnapshot> {
    if let Some(task) = crate::tasks::local_shell_task::task_identity_snapshot(task_id, app_store) {
        return Some(task);
    }
    if let Some(task) = crate::tasks::local_agent_task::task_identity_snapshot(task_id) {
        return Some(TaskLookupSnapshot {
            status: task.status,
            task_type: task.task_type,
            command: Some(task.description.clone()),
            description: task.description,
        });
    }
    if let Some(task) = crate::tasks::in_process_teammate_task::task_identity_snapshot(task_id) {
        return Some(TaskLookupSnapshot {
            status: task.status,
            task_type: task.task_type,
            command: Some(task.description.clone()),
            description: task.description,
        });
    }

    let state = app_store?.get();
    match state.tasks.get(task_id)?.as_ref() {
        crate::state::app_state_store::TaskState::InProcessTeammate(task) => {
            Some(TaskLookupSnapshot {
                status: task.status.clone(),
                task_type: task.task_type.clone(),
                description: task.agent_name.clone(),
                command: Some(task.agent_name.clone()),
            })
        }
        crate::state::app_state_store::TaskState::LocalShell(task) => Some(TaskLookupSnapshot {
            status: task.status.clone(),
            task_type: task.task_type.clone(),
            description: task.description.clone(),
            command: Some(task.command.clone()),
        }),
        // CC stopTask.ts reads the same TaskStateBase fields off any union
        // member; dream carries no command (`:97` falls back to description).
        crate::state::app_state_store::TaskState::Dream(task) => Some(TaskLookupSnapshot {
            status: task.status.clone(),
            task_type: task.task_type.clone(),
            description: task.description.clone(),
            command: Some(task.description.clone()),
        }),
        crate::state::app_state_store::TaskState::Other(task) => Some(TaskLookupSnapshot {
            status: task.status.clone(),
            task_type: task.task_type.clone(),
            description: task.description.clone(),
            command: Some(task.description.clone()),
        }),
    }
}

/// Look up a task, validate it is running, dispatch its concrete kill
/// implementation, and preserve the official shell-only notification
/// suppression rule.
///
/// Maps to CC `tasks/stopTask.ts:38-73`.
pub async fn stop_task(
    task_id: &str,
    app_store: Option<&AppStore>,
) -> Result<StopTaskResult, StopTaskError> {
    let Some(task) = lookup_task(task_id, app_store) else {
        return Err(StopTaskError::new(
            format!("No task found with ID: {task_id}"),
            StopTaskErrorCode::NotFound,
        ));
    };
    if task.status != "running" {
        return Err(not_running(task_id, &task.status));
    }

    let killed = match task.task_type.as_str() {
        "local_bash" => {
            crate::tasks::local_shell_task::kill_shell_tasks::kill_task(task_id, app_store)
        }
        "local_agent" => crate::tasks::local_agent_task::kill_async_agent(task_id),
        "in_process_teammate" => {
            crate::tasks::in_process_teammate_task::kill_in_process_teammate(task_id)
        }
        // CC stopTask.ts:57-64 dispatches through getTaskByType, which covers
        // DreamTask.kill (`DreamTask.ts:132-157`).
        "dream" => app_store
            .map(|store| crate::tasks::dream_task::kill_dream_task(task_id, store))
            .unwrap_or(false),
        _ => {
            return Err(StopTaskError::new(
                format!("Unsupported task type: {}", task.task_type),
                StopTaskErrorCode::UnsupportedType,
            ));
        }
    };

    if !killed {
        if let Some(current) = lookup_task(task_id, app_store)
            && current.status != "running"
        {
            return Err(not_running(task_id, &current.status));
        }
        return Err(StopTaskError::new(
            format!("Unsupported task type: {}", task.task_type),
            StopTaskErrorCode::UnsupportedType,
        ));
    }

    Ok(StopTaskResult {
        task_id: task_id.to_string(),
        task_type: task.task_type,
        command: task.command,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_task_kills_shell_and_suppresses_completion_notification_like_official() {
        let task_id = format!("task_{}", uuid::Uuid::new_v4().simple());
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        store.replace_with(|state| {
            std::sync::Arc::make_mut(&mut state.tasks).insert(
                task_id.clone(),
                std::sync::Arc::new(crate::state::app_state_store::TaskState::LocalShell(
                    crate::tasks::local_shell_task::guards::LocalShellTaskState {
                        id: task_id.clone(),
                        task_type: "local_bash".to_string(),
                        command: "sleep 30".to_string(),
                        description: "wait".to_string(),
                        status: "running".to_string(),
                        result: None,
                        notified: false,
                        shell_command: None,
                        last_reported_total_lines: 0,
                        is_backgrounded: true,
                        agent_id: None,
                        tool_use_id: Some("toolu_shell".to_string()),
                        kind: None,
                        start_time_ms: 0,
                        end_time_ms: None,
                    },
                )),
            );
        });

        let result = stop_task(&task_id, Some(&store)).await.unwrap();
        assert_eq!(result.task_type, "local_bash");
        assert_eq!(result.command.as_deref(), Some("sleep 30"));
        let state = store.get();
        let crate::state::app_state_store::TaskState::LocalShell(task) =
            state.tasks.get(&task_id).unwrap().as_ref()
        else {
            panic!("expected local shell task");
        };
        assert_eq!(task.status, "killed");
        assert!(task.notified);
    }

    #[tokio::test]
    async fn stop_task_keeps_agent_notification_available_for_abort_catch() {
        let _lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
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
                tool_use_id: Some("toolu_agent".to_string()),
            },
        );

        let result = stop_task(&task_id, None).await.unwrap();
        assert_eq!(result.task_type, "local_agent");
        let task = crate::tasks::local_agent_task::get_local_agent_task(&task_id).unwrap();
        assert_eq!(task.status, "killed");
        assert!(task.abort_controller.is_aborted());
        assert!(
            !task.notified,
            "agent abort catch owns the partial-result notification"
        );

        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
    }

    #[tokio::test]
    async fn stop_task_dispatches_in_process_teammate_kill_like_official_union() {
        let _lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        let task_id = format!("teammate-{}", uuid::Uuid::new_v4());
        crate::tasks::in_process_teammate_task::register_in_process_teammate_task(
            crate::tasks::in_process_teammate_task::RegisterInProcessTeammateParams {
                task_id: task_id.clone(),
                identity: crate::tasks::in_process_teammate_task::TeammateIdentity {
                    agent_id: "reviewer@team".to_string(),
                    agent_name: "reviewer".to_string(),
                    team_name: "team".to_string(),
                    color: None,
                    plan_mode_required: false,
                    parent_session_id: "parent".to_string(),
                },
                description: "review changes".to_string(),
                prompt: "review".to_string(),
                selected_agent: None,
                model: None,
                permission_mode: crate::types::permissions::PermissionMode::Default,
                tool_use_id: Some("toolu_teammate".to_string()),
            },
        );

        let result = stop_task(&task_id, None).await.unwrap();
        assert_eq!(result.task_type, "in_process_teammate");
        assert_eq!(result.command.as_deref(), Some("review changes"));
        let task =
            crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id).unwrap();
        assert_eq!(task.status, "killed");
        assert!(task.abort_controller.unwrap().is_aborted());
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
    }
}
