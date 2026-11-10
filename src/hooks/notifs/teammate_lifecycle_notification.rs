//! Maps to: CC `hooks/notifs/useTeammateShutdownNotification.ts`.
//!
//! The official hook watches live in-process teammate tasks and adds folded
//! notifications such as `3 agents spawned`. Nyan does not have the live task
//! store wired yet, so this file exposes the same notification producer shape as
//! pure constructors for future task-state integration.

#![allow(dead_code)]

use crate::context::notifications::{
    Notification, NotificationFold, NotificationPriority, count_prefix_text,
};
use std::collections::HashSet;

const TEAMMATE_LIFECYCLE_TIMEOUT_MS: u64 = 5_000;
const IN_PROCESS_TEAMMATE_TASK_TYPE: &str = "in_process_teammate";
const RUNNING_TASK_STATUS: &str = "running";
const COMPLETED_TASK_STATUS: &str = "completed";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeammateLifecycleTaskSnapshot {
    pub id: String,
    pub task_type: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeammateLifecycleNotificationState {
    pub seen_running_ids: HashSet<String>,
    pub seen_completed_ids: HashSet<String>,
}

fn count_prefix_fold(singular: &str, plural: &str) -> NotificationFold {
    NotificationFold::CountPrefix {
        singular: singular.to_string(),
        plural: plural.to_string(),
    }
}

pub fn teammate_spawn_notification(count: u32) -> Notification {
    Notification::text(
        "teammate-spawn",
        count_prefix_text(count, "agent spawned", "agents spawned"),
        NotificationPriority::Low,
    )
    .with_timeout_ms(TEAMMATE_LIFECYCLE_TIMEOUT_MS)
    .with_fold(count_prefix_fold("agent spawned", "agents spawned"))
}

pub fn teammate_shutdown_notification(count: u32) -> Notification {
    Notification::text(
        "teammate-shutdown",
        count_prefix_text(count, "agent shut down", "agents shut down"),
        NotificationPriority::Low,
    )
    .with_timeout_ms(TEAMMATE_LIFECYCLE_TIMEOUT_MS)
    .with_fold(count_prefix_fold("agent shut down", "agents shut down"))
}

/// Pure counterpart of official `useTeammateLifecycleNotification()`.
///
/// The official hook reads the live AppState task store and mutates two ref-held
/// seen sets before enqueueing one folded notification per new spawn/shutdown.
/// This planner accepts an explicit task snapshot plus caller-owned seen state
/// so future runtime wiring can reuse the exact producer semantics without
/// starting task execution or writing session data.
pub fn teammate_lifecycle_notifications_from_tasks<'a>(
    state: &mut TeammateLifecycleNotificationState,
    tasks: impl IntoIterator<Item = &'a TeammateLifecycleTaskSnapshot>,
) -> Vec<Notification> {
    let mut notifications = Vec::new();
    for task in tasks {
        if task.task_type != IN_PROCESS_TEAMMATE_TASK_TYPE {
            continue;
        }

        match task.status.trim() {
            RUNNING_TASK_STATUS if state.seen_running_ids.insert(task.id.clone()) => {
                notifications.push(teammate_spawn_notification(1));
            }
            COMPLETED_TASK_STATUS if state.seen_completed_ids.insert(task.id.clone()) => {
                notifications.push(teammate_shutdown_notification(1));
            }
            _ => {}
        }
    }

    notifications
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::notifications::NotificationsState;

    #[test]
    fn teammate_lifecycle_notifications_match_official_text_and_timeout() {
        let spawn = teammate_spawn_notification(1);
        let shutdown = teammate_shutdown_notification(2);

        assert_eq!(spawn.key, "teammate-spawn");
        assert_eq!(spawn.text, "1 agent spawned");
        assert_eq!(spawn.priority, NotificationPriority::Low);
        assert_eq!(spawn.timeout_ms, Some(5_000));
        assert!(spawn.fold.is_some());

        assert_eq!(shutdown.key, "teammate-shutdown");
        assert_eq!(shutdown.text, "2 agents shut down");
        assert_eq!(shutdown.priority, NotificationPriority::Low);
        assert_eq!(shutdown.timeout_ms, Some(5_000));
        assert!(shutdown.fold.is_some());
    }

    #[test]
    fn teammate_lifecycle_notifications_fold_repeated_events() {
        let mut notifications = NotificationsState::default();

        notifications.add(teammate_spawn_notification(1), |_, _| {});
        notifications.add(teammate_spawn_notification(1), |_, _| {});
        notifications.add(teammate_spawn_notification(1), |_, _| {});

        // Folding happens inside the mutation; promotion does not (CC's
        // `processQueue()` is the separate trailing transition), so the folded
        // notification is observed in the queue.
        assert_eq!(
            notifications
                .queue
                .first()
                .map(|notification| notification.text.as_str()),
            Some("3 agents spawned")
        );
    }

    #[test]
    fn teammate_lifecycle_task_planner_matches_official_seen_set_semantics() {
        let mut state = TeammateLifecycleNotificationState::default();
        let tasks = vec![
            TeammateLifecycleTaskSnapshot {
                id: "agent-a".to_string(),
                task_type: IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
                status: RUNNING_TASK_STATUS.to_string(),
            },
            TeammateLifecycleTaskSnapshot {
                id: "agent-b".to_string(),
                task_type: IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
                status: RUNNING_TASK_STATUS.to_string(),
            },
            TeammateLifecycleTaskSnapshot {
                id: "agent-c".to_string(),
                task_type: IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
                status: COMPLETED_TASK_STATUS.to_string(),
            },
            TeammateLifecycleTaskSnapshot {
                id: "external-task".to_string(),
                task_type: "external".to_string(),
                status: RUNNING_TASK_STATUS.to_string(),
            },
        ];

        let first = teammate_lifecycle_notifications_from_tasks(&mut state, &tasks);
        assert_eq!(
            first
                .iter()
                .map(|notification| notification.text.as_str())
                .collect::<Vec<_>>(),
            vec!["1 agent spawned", "1 agent spawned", "1 agent shut down"]
        );

        let second = teammate_lifecycle_notifications_from_tasks(&mut state, &tasks);
        assert!(
            second.is_empty(),
            "already-seen running/completed tasks must not emit duplicates"
        );

        let completed_agent_a = vec![TeammateLifecycleTaskSnapshot {
            id: "agent-a".to_string(),
            task_type: IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
            status: COMPLETED_TASK_STATUS.to_string(),
        }];
        let shutdown = teammate_lifecycle_notifications_from_tasks(&mut state, &completed_agent_a);
        assert_eq!(shutdown.len(), 1);
        assert_eq!(shutdown[0].text, "1 agent shut down");
    }
}
