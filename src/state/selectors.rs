//! Maps to: CC `state/selectors.ts`.
//!
//! Pure derived views over [`AppState`]. Keep side-effect free.

use super::app_state_store::{AppState, TaskState, TaskStateOther};
use crate::components::spinner::teammate_tree::TeammateTaskSnapshot;

/// Maps to: CC `getViewedTeammateTask`.
pub fn get_viewed_teammate_task(app_state: &AppState) -> Option<&TeammateTaskSnapshot> {
    let task_id = app_state.viewing_agent_task_id.as_deref()?;
    app_state
        .tasks
        .get(task_id)
        .and_then(|task| task.as_in_process_teammate())
}

/// Maps to: CC `ActiveAgentForInput`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveAgentForInput<'a> {
    Leader,
    Viewed { task: &'a TeammateTaskSnapshot },
    NamedAgent { task: &'a TaskStateOther },
}

/// Maps to: CC `getActiveAgentForInput`.
pub fn get_active_agent_for_input(app_state: &AppState) -> ActiveAgentForInput<'_> {
    if let Some(task) = get_viewed_teammate_task(app_state) {
        return ActiveAgentForInput::Viewed { task };
    }

    if let Some(task_id) = app_state.viewing_agent_task_id.as_deref()
        && let Some(TaskState::Other(task)) = app_state.tasks.get(task_id).map(|task| task.as_ref())
        && task.task_type == "local_agent"
    {
        return ActiveAgentForInput::NamedAgent { task };
    }

    ActiveAgentForInput::Leader
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::{AppState, TaskState, TaskStateOther};

    fn teammate(id: &str, name: &str) -> TeammateTaskSnapshot {
        TeammateTaskSnapshot {
            id: id.to_string(),
            agent_name: name.to_string(),
            task_type: "in_process_teammate".to_string(),
            status: "running".to_string(),
            ..TeammateTaskSnapshot::default()
        }
    }

    fn insert_task(state: &mut AppState, id: &str, task: TaskState) {
        std::sync::Arc::make_mut(&mut state.tasks)
            .insert(id.to_string(), std::sync::Arc::new(task));
    }

    #[test]
    fn get_viewed_teammate_task_requires_in_process_teammate() {
        let mut state = AppState::default();
        assert!(get_viewed_teammate_task(&state).is_none());

        state.viewing_agent_task_id = Some("t1".into());
        insert_task(
            &mut state,
            "t1",
            TaskState::InProcessTeammate(teammate("t1", "worker")),
        );
        assert_eq!(
            get_viewed_teammate_task(&state).map(|t| t.agent_name.as_str()),
            Some("worker")
        );

        insert_task(
            &mut state,
            "t1",
            TaskState::Other(TaskStateOther {
                id: "t1".into(),
                task_type: "local_agent".into(),
                status: "running".into(),
                description: "agent".into(),
                is_backgrounded: None,
                notified: false,
                retain: None,
                evict_after: None,
                progress_tool_uses: None,
                progress_tokens: None,
            }),
        );
        assert!(get_viewed_teammate_task(&state).is_none());
    }

    #[test]
    fn get_active_agent_for_input_routes_leader_viewed_named() {
        let mut state = AppState::default();
        assert_eq!(
            get_active_agent_for_input(&state),
            ActiveAgentForInput::Leader
        );

        state.viewing_agent_task_id = Some("t1".into());
        insert_task(
            &mut state,
            "t1",
            TaskState::InProcessTeammate(teammate("t1", "worker")),
        );
        assert!(matches!(
            get_active_agent_for_input(&state),
            ActiveAgentForInput::Viewed { .. }
        ));

        insert_task(
            &mut state,
            "t1",
            TaskState::Other(TaskStateOther {
                id: "t1".into(),
                task_type: "local_agent".into(),
                status: "running".into(),
                description: "named".into(),
                is_backgrounded: None,
                notified: false,
                retain: None,
                evict_after: None,
                progress_tool_uses: None,
                progress_tokens: None,
            }),
        );
        assert!(matches!(
            get_active_agent_for_input(&state),
            ActiveAgentForInput::NamedAgent { .. }
        ));
    }
}
