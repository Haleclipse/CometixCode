//! Maps to: CC `tasks/pillLabel.ts:1-83`.
//!
//! Compact footer-pill label for a set of background tasks. Used by both the
//! footer pill and the turn-duration transcript line so the two surfaces agree
//! on terminology (CC pillLabel.ts:5-9).

/// Projection of CC's `BackgroundTaskState` union (`tasks/types.ts:22-29`)
/// carrying only the fields `getPillLabel` reads. Rust has no
/// unified rich task union yet; precedent for the projection-enum shape is
/// `FooterTaskProjection` in `components/tasks/task_status_utils.rs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PillTask {
    /// CC `LocalShellTaskState`; `kind === 'monitor'` splits the count
    /// (pillLabel.ts:16-28).
    LocalBash { is_monitor: bool },
    /// CC `InProcessTeammateTaskState`; `identity.teamName` deduplicates
    /// (pillLabel.ts:29-36).
    InProcessTeammate { team_name: String },
    /// CC `LocalAgentTaskState` (pillLabel.ts:37-38).
    LocalAgent,
    /// CC `LocalWorkflowTaskState` (pillLabel.ts:57-58).
    /// SEAM (producer missing): the ant WORKFLOW_SCRIPTS task type has no Rust
    /// producer.
    LocalWorkflow,
    /// CC `MonitorMcpTaskState` (pillLabel.ts:59-60).
    /// SEAM (producer missing): no Rust producer.
    MonitorMcp,
    /// CC `DreamTaskState` (pillLabel.ts:61-62); produced by
    /// `tasks/dream_task.rs` (driver `services/autoDream` still unported).
    Dream,
}

fn same_variant(a: &PillTask, b: &PillTask) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// Maps to: CC `tasks/pillLabel.ts:10-67` `getPillLabel`.
pub fn get_pill_label(tasks: &[PillTask]) -> String {
    let n = tasks.len();
    // CC `:12` — `tasks.every(t => t.type === tasks[0]!.type)`.
    let all_same_type = tasks
        .windows(2)
        .all(|pair| same_variant(&pair[0], &pair[1]));

    // CC `:14-63` — the per-type switch. CC never calls this with an empty
    // list (both call sites gate on length > 0, BackgroundTaskStatus.tsx:195
    // and SystemTextMessage.tsx:357); the total Rust function falls through to
    // the `:66` fallback ("0 background tasks") instead of CC's TypeError.
    if let Some(first) = tasks.first()
        && all_same_type
    {
        match first {
            PillTask::LocalBash { .. } => {
                // CC `:16-28` — shells/monitors split by `kind`.
                let monitors = tasks
                    .iter()
                    .filter(|task| matches!(task, PillTask::LocalBash { is_monitor: true }))
                    .count();
                let shells = n - monitors;
                let mut parts = Vec::new();
                if shells > 0 {
                    parts.push(if shells == 1 {
                        "1 shell".to_string()
                    } else {
                        format!("{shells} shells")
                    });
                }
                if monitors > 0 {
                    parts.push(if monitors == 1 {
                        "1 monitor".to_string()
                    } else {
                        format!("{monitors} monitors")
                    });
                }
                return parts.join(", ");
            }
            PillTask::InProcessTeammate { .. } => {
                // CC `:29-36` — distinct team names via a Set.
                let team_count = tasks
                    .iter()
                    .filter_map(|task| match task {
                        PillTask::InProcessTeammate { team_name } => Some(team_name.as_str()),
                        _ => None,
                    })
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                return if team_count == 1 {
                    "1 team".to_string()
                } else {
                    format!("{team_count} teams")
                };
            }
            PillTask::LocalAgent => {
                // CC `:37-38`.
                return if n == 1 {
                    "1 local agent".to_string()
                } else {
                    format!("{n} local agents")
                };
            }
            PillTask::LocalWorkflow => {
                // CC `:57-58`.
                return if n == 1 {
                    "1 background workflow".to_string()
                } else {
                    format!("{n} background workflows")
                };
            }
            PillTask::MonitorMcp => {
                // CC `:59-60`.
                return if n == 1 {
                    "1 monitor".to_string()
                } else {
                    format!("{n} monitors")
                };
            }
            PillTask::Dream => {
                // CC `:61-62`.
                return "dreaming".to_string();
            }
        }
    }

    // CC `:66` — mixed-type fallback.
    format!("{n} background {}", if n == 1 { "task" } else { "tasks" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_bash_splits_shells_and_monitors_with_pluralization() {
        // CC pillLabel.ts:16-28.
        assert_eq!(
            get_pill_label(&[PillTask::LocalBash { is_monitor: false }]),
            "1 shell"
        );
        assert_eq!(
            get_pill_label(&[
                PillTask::LocalBash { is_monitor: false },
                PillTask::LocalBash { is_monitor: false },
            ]),
            "2 shells"
        );
        assert_eq!(
            get_pill_label(&[PillTask::LocalBash { is_monitor: true }]),
            "1 monitor"
        );
        assert_eq!(
            get_pill_label(&[
                PillTask::LocalBash { is_monitor: false },
                PillTask::LocalBash { is_monitor: true },
                PillTask::LocalBash { is_monitor: true },
            ]),
            "1 shell, 2 monitors"
        );
    }

    #[test]
    fn teammates_count_distinct_team_names() {
        // CC pillLabel.ts:29-36 — Set over identity.teamName.
        assert_eq!(
            get_pill_label(&[
                PillTask::InProcessTeammate {
                    team_name: "alpha".to_string()
                },
                PillTask::InProcessTeammate {
                    team_name: "alpha".to_string()
                },
            ]),
            "1 team"
        );
        assert_eq!(
            get_pill_label(&[
                PillTask::InProcessTeammate {
                    team_name: "alpha".to_string()
                },
                PillTask::InProcessTeammate {
                    team_name: "beta".to_string()
                },
            ]),
            "2 teams"
        );
    }

    #[test]
    fn local_agent_workflow_monitor_and_dream_labels_match_official() {
        assert_eq!(get_pill_label(&[PillTask::LocalAgent]), "1 local agent");
        assert_eq!(
            get_pill_label(&[PillTask::LocalAgent, PillTask::LocalAgent]),
            "2 local agents"
        );
        assert_eq!(
            get_pill_label(&[PillTask::LocalWorkflow]),
            "1 background workflow"
        );
        assert_eq!(
            get_pill_label(&[PillTask::LocalWorkflow, PillTask::LocalWorkflow]),
            "2 background workflows"
        );
        assert_eq!(get_pill_label(&[PillTask::MonitorMcp]), "1 monitor");
        assert_eq!(
            get_pill_label(&[PillTask::MonitorMcp, PillTask::MonitorMcp]),
            "2 monitors"
        );
        assert_eq!(get_pill_label(&[PillTask::Dream]), "dreaming");
        assert_eq!(
            get_pill_label(&[PillTask::Dream, PillTask::Dream]),
            "dreaming"
        );
    }

    #[test]
    fn mixed_types_fall_back_to_background_task_count() {
        // CC pillLabel.ts:66.
        assert_eq!(
            get_pill_label(&[
                PillTask::LocalBash { is_monitor: false },
                PillTask::LocalAgent,
            ]),
            "2 background tasks"
        );
        // Empty input never happens in CC (both call sites gate on length);
        // the Rust total function yields the fallback.
        assert_eq!(get_pill_label(&[]), "0 background tasks");
    }
}
