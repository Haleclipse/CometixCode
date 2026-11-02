//! In-process teammate spawning.
//!
//! Maps to: CC `utils/swarm/spawnInProcess.ts`.
//!
//! This module creates the official teammate identity/context and registers an
//! `InProcessTeammateTaskState`. It intentionally does **not** execute the
//! teammate loop; CC delegates that to `utils/swarm/inProcessRunner.ts` via
//! `startInProcessTeammate(...)`, which is still a separate parity slice.

use crate::tasks::in_process_teammate_task::{
    InProcessTeammateTaskState, RegisterInProcessTeammateParams, TeammateIdentity,
    register_in_process_teammate_task,
};
use crate::tool::AbortController;
use crate::types::permissions::PermissionMode;
use crate::utils::agent_id::format_agent_id;
use crate::utils::teammate_context::{CreateTeammateContextConfig, create_teammate_context};

/// Maps to: CC `utils/swarm/spawnInProcess.ts#SpawnContext`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnContext {
    pub tool_use_id: Option<String>,
}

/// Maps to: CC `utils/swarm/spawnInProcess.ts#InProcessSpawnConfig`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InProcessSpawnConfig {
    pub name: String,
    pub team_name: String,
    pub prompt: String,
    pub color: Option<String>,
    pub plan_mode_required: bool,
    pub model: Option<String>,
}

/// Maps to: CC `utils/swarm/spawnInProcess.ts#InProcessSpawnOutput`.
#[derive(Clone, Debug, PartialEq)]
pub struct InProcessSpawnOutput {
    pub success: bool,
    pub agent_id: String,
    pub task_id: Option<String>,
    pub abort_controller: Option<AbortController>,
    pub teammate_context: Option<crate::utils::teammate_context::TeammateContext>,
    pub task_state: Option<InProcessTeammateTaskState>,
    pub error: Option<String>,
}

/// Source-shaped adapter to the canonical `Task.ts#generateTaskId` owner.
pub fn generate_in_process_teammate_task_id() -> String {
    crate::task::generate_task_id(crate::task::TaskType::InProcessTeammate)
}

fn teammate_description(name: &str, prompt: &str) -> String {
    let prefix = prompt.chars().take(50).collect::<String>();
    let ellipsis = if prompt.chars().count() > 50 {
        "..."
    } else {
        ""
    };
    format!("{name}: {prefix}{ellipsis}")
}

/// Maps to: CC `utils/swarm/spawnInProcess.ts#spawnInProcessTeammate`.
pub async fn spawn_in_process_teammate(
    config: InProcessSpawnConfig,
    context: SpawnContext,
) -> InProcessSpawnOutput {
    let agent_id = format_agent_id(&config.name, &config.team_name);
    let task_id = generate_in_process_teammate_task_id();

    let parent_session_id = crate::bootstrap::state::get_session_id();
    let identity = TeammateIdentity {
        agent_id: agent_id.clone(),
        agent_name: config.name.clone(),
        team_name: config.team_name.clone(),
        color: config.color.clone(),
        plan_mode_required: config.plan_mode_required,
        parent_session_id: parent_session_id.clone(),
    };

    let task_state = register_in_process_teammate_task(RegisterInProcessTeammateParams {
        task_id: task_id.clone(),
        identity: identity.clone(),
        description: teammate_description(&config.name, &config.prompt),
        prompt: config.prompt.clone(),
        selected_agent: None,
        model: config.model.clone(),
        permission_mode: if config.plan_mode_required {
            PermissionMode::Plan
        } else {
            PermissionMode::Default
        },
        tool_use_id: context.tool_use_id,
    });

    let abort_controller = task_state.abort_controller.clone().unwrap_or_default();
    let teammate_context = create_teammate_context(CreateTeammateContextConfig {
        agent_id: agent_id.clone(),
        agent_name: config.name,
        team_name: config.team_name,
        color: config.color,
        plan_mode_required: config.plan_mode_required,
        parent_session_id,
        abort_controller: abort_controller.clone(),
    });

    InProcessSpawnOutput {
        success: true,
        agent_id,
        task_id: Some(task_id),
        abort_controller: Some(abort_controller),
        teammate_context: Some(teammate_context),
        task_state: Some(task_state),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_task_id_matches_official_in_process_prefix_shape() {
        let id = generate_in_process_teammate_task_id();
        assert_eq!(id.len(), 9);
        assert!(id.starts_with('t'));
        assert!(
            id[1..]
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        );
    }

    #[tokio::test]
    async fn spawn_in_process_teammate_registers_task_and_context_like_official() {
        let _lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();

        let output = spawn_in_process_teammate(
            InProcessSpawnConfig {
                name: "researcher".to_string(),
                team_name: "alpha".to_string(),
                prompt: "investigate the failing tests and report back with concise findings"
                    .to_string(),
                color: Some("blue".to_string()),
                plan_mode_required: true,
                model: Some("sonnet".to_string()),
            },
            SpawnContext {
                tool_use_id: Some("toolu_spawn".to_string()),
            },
        )
        .await;

        assert!(output.success);
        assert_eq!(output.agent_id, "researcher@alpha");
        let task_id = output.task_id.clone().unwrap();
        let task = crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
            .expect("task registered");
        assert_eq!(task.task_type, "in_process_teammate");
        assert_eq!(task.status, "running");
        assert_eq!(task.identity.agent_id, "researcher@alpha");
        assert_eq!(task.permission_mode, PermissionMode::Plan);
        assert_eq!(task.model.as_deref(), Some("sonnet"));
        assert_eq!(task.tool_use_id.as_deref(), Some("toolu_spawn"));
        assert!(task.description.starts_with("researcher: investigate"));
        assert!(!task.output_file.is_empty());

        let teammate_context = output.teammate_context.unwrap();
        assert!(teammate_context.is_in_process);
        assert_eq!(teammate_context.agent_id, "researcher@alpha");
        let abort = output.abort_controller.unwrap();
        abort.abort();
        assert!(teammate_context.abort_controller.is_aborted());
    }
}
