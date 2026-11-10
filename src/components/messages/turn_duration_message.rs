//! Extracted Rust component for the nested `TurnDurationMessage` helper in
//! CC `components/messages/SystemTextMessage.tsx`.

use crate::constants::figures::TEARDROP_ASTERISK;
use crate::tasks::pill_label::PillTask;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Mount-time projection of `AppState.tasks` into pill inputs.
///
/// Maps to: CC `SystemTextMessage.tsx:352-357` — `Object.values(tasks)` filtered
/// by `isBackgroundTask` and fed to `getPillLabel`; the projection enum stands
/// in for CC's `BackgroundTaskState` union (`tasks/pill_label.rs`).
fn pill_tasks_from_app_state(state: &crate::state::app_state_store::AppState) -> Vec<PillTask> {
    use crate::state::app_state_store::TaskState;
    state
        .tasks
        .values()
        .filter(|task| crate::tasks::types::is_background_task(task))
        .filter_map(|task| match task.as_ref() {
            TaskState::LocalShell(shell) => Some(PillTask::LocalBash {
                is_monitor: shell.kind.as_deref() == Some("monitor"),
            }),
            TaskState::InProcessTeammate(_) => Some(PillTask::InProcessTeammate {
                // TeammateTaskSnapshot does not project identity.teamName; all
                // in-process teammates share one team per session today, so the
                // empty-string key still dedupes to CC's "1 team"
                // (pillLabel.ts:29-36).
                team_name: String::new(),
            }),
            TaskState::Dream(_) => Some(PillTask::Dream),
            TaskState::Other(other) => match other.task_type.as_str() {
                "local_agent" => Some(PillTask::LocalAgent),
                "local_workflow" => Some(PillTask::LocalWorkflow),
                "monitor_mcp" => Some(PillTask::MonitorMcp),
                _ => None,
            },
        })
        .collect()
}

#[derive(Default, Props)]
pub struct TurnDurationMessageProps {
    pub duration: String,
    pub duration_ms: Option<u64>,
    pub budget_tokens: Option<u64>,
    pub budget_limit: Option<u64>,
    pub budget_nudges: u64,
    pub add_margin: bool,
}

#[component]
pub fn TurnDurationMessage(
    props: &TurnDurationMessageProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    // Maps to: CC SystemTextMessage.tsx:351-358 — a MOUNT-TIME snapshot
    // (useState initializer) of the running background tasks; the label is
    // frozen for the life of the transcript row.
    let app_store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    let background_task_summary = hooks.use_state(move || {
        let running = app_store
            .map(|store| pill_tasks_from_app_state(&store.get()))
            .unwrap_or_default();
        (!running.is_empty()).then(|| crate::tasks::pill_label::get_pill_label(&running))
    });
    let duration = if props.duration.trim().is_empty() {
        props
            .duration_ms
            .map(format_duration_ms)
            .unwrap_or_default()
    } else {
        props.duration.clone()
    };
    let budget_suffix = budget_suffix(
        props.budget_tokens,
        props.budget_limit,
        props.budget_nudges,
        !duration.trim().is_empty(),
    );

    if duration.trim().is_empty() && budget_suffix.is_empty() {
        return element! { View }.into_any();
    }

    // Maps to: CC SystemTextMessage.tsx:396-397 —
    // `` ` · ${backgroundTaskSummary} still running` ``.
    let summary_suffix = background_task_summary
        .read()
        .as_ref()
        .map(|summary| format!(" · {summary} still running"))
        .unwrap_or_default();
    let text = if duration.trim().is_empty() {
        format!("{budget_suffix}{summary_suffix}")
    } else {
        format!("Worked for {duration}{budget_suffix}{summary_suffix}")
    };

    element! {
        View(
            flex_direction: FlexDirection::Row,
            margin_top: if props.add_margin { 1u32 } else { 0u32 },
            width: 100pct,
        ) {
            View(min_width: 2u32, flex_shrink: 0.0f32) {
                Text(content: TEARDROP_ASTERISK.to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
            }
            Text(content: text, color: theme.inactive)
        }
    }
    .into_any()
}

fn budget_suffix(
    budget_tokens: Option<u64>,
    budget_limit: Option<u64>,
    budget_nudges: u64,
    has_duration: bool,
) -> String {
    let Some(limit) = budget_limit else {
        return String::new();
    };
    let tokens = budget_tokens.unwrap_or(0);
    let usage = if tokens >= limit {
        format!(
            "{} used ({} min ✓)",
            format_compact_number(tokens),
            format_compact_number(limit)
        )
    } else if limit == 0 {
        format!("{} / 0 (0%)", format_compact_number(tokens))
    } else {
        let percent = ((tokens as f64 / limit as f64) * 100.0).round() as u64;
        format!(
            "{} / {} ({}%)",
            format_compact_number(tokens),
            format_compact_number(limit),
            percent
        )
    };
    let nudges = if budget_nudges > 0 {
        format!(
            " · {budget_nudges} {}",
            if budget_nudges == 1 {
                "nudge"
            } else {
                "nudges"
            }
        )
    } else {
        String::new()
    };
    format!(
        "{}{}{}",
        if has_duration { " · " } else { "" },
        usage,
        nudges
    )
}

fn format_compact_number(value: u64) -> String {
    fn one_decimal(value: f64, suffix: &str) -> String {
        let rendered = format!("{value:.1}");
        format!(
            "{}{}",
            rendered.strip_suffix(".0").unwrap_or(&rendered),
            suffix
        )
    }

    if value >= 1_000_000 {
        one_decimal(value as f64 / 1_000_000.0, "M")
    } else if value >= 1_000 {
        one_decimal(value as f64 / 1_000.0, "K")
    } else {
        value.to_string()
    }
}

fn format_duration_ms(ms: u64) -> String {
    if ms < 60_000 {
        return format!("{}s", ms / 1000);
    }

    let mut seconds = ((ms % 60_000) as f64 / 1000.0).round() as u64;
    let mut minutes_total = ms / 60_000;
    if seconds == 60 {
        seconds = 0;
        minutes_total += 1;
    }

    let days = minutes_total / (24 * 60);
    let hours = (minutes_total % (24 * 60)) / 60;
    let minutes = minutes_total % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_turn_duration(
        duration: &str,
        duration_ms: Option<u64>,
        budget_tokens: Option<u64>,
        budget_limit: Option<u64>,
        budget_nudges: u64,
    ) -> String {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                TurnDurationMessage(
                    duration: duration.to_string(),
                    duration_ms: duration_ms,
                    budget_tokens: budget_tokens,
                    budget_limit: budget_limit,
                    budget_nudges: budget_nudges,
                )
            }
        }
        .render(None)
        .to_string()
    }

    #[test]
    fn turn_duration_formats_duration_ms_like_official_system_message() {
        assert_eq!(format_duration_ms(0), "0s");
        assert_eq!(format_duration_ms(1_234), "1s");
        assert_eq!(format_duration_ms(61_400), "1m 1s");
        assert_eq!(format_duration_ms(3_661_000), "1h 1m 1s");

        let rendered = render_turn_duration("", Some(1_234), None, None, 0);
        assert!(rendered.contains("✻"));
        assert!(rendered.contains("Worked for 1s"));
    }

    #[test]
    fn turn_duration_renders_budget_suffixes_from_official_fields() {
        let partial = render_turn_duration("5s", None, Some(1_200), Some(4_000), 1);
        assert!(partial.contains("Worked for 5s · 1.2K / 4K (30%) · 1 nudge"));

        let exhausted = render_turn_duration("5s", None, Some(4_200), Some(4_000), 2);
        assert!(exhausted.contains("4.2K used (4K min ✓) · 2 nudges"));
    }

    fn other_task(
        id: &str,
        task_type: &str,
        status: &str,
        is_backgrounded: Option<bool>,
    ) -> crate::state::app_state_store::TaskState {
        crate::state::app_state_store::TaskState::Other(
            crate::state::app_state_store::TaskStateOther {
                id: id.to_string(),
                task_type: task_type.to_string(),
                status: status.to_string(),
                description: String::new(),
                is_backgrounded,
                notified: false,
                retain: Some(false),
                evict_after: None,
                progress_tool_uses: None,
                progress_tokens: None,
            },
        )
    }

    #[test]
    fn pill_projection_filters_by_is_background_task_and_maps_types() {
        // CC SystemTextMessage.tsx:352-357 filter + the union projection.
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        store.replace_with(|state| {
            let tasks = std::sync::Arc::make_mut(&mut state.tasks);
            for (id, task) in [
                (
                    "agent-running",
                    other_task("agent-running", "local_agent", "running", Some(true)),
                ),
                (
                    "agent-foreground",
                    other_task("agent-foreground", "local_agent", "running", Some(false)),
                ),
                (
                    "agent-done",
                    other_task("agent-done", "local_agent", "completed", Some(true)),
                ),
            ] {
                tasks.insert(id.to_string(), std::sync::Arc::new(task));
            }
        });
        let pills = pill_tasks_from_app_state(&store.get());
        assert_eq!(pills, vec![PillTask::LocalAgent]);
    }

    #[test]
    fn turn_duration_appends_running_background_task_summary_from_mount_snapshot() {
        // CC SystemTextMessage.tsx:352-358 (mount snapshot) + :396-397
        // (` · ${summary} still running`).
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        store.replace_with(|state| {
            std::sync::Arc::make_mut(&mut state.tasks).insert(
                "agent-1".to_string(),
                std::sync::Arc::new(other_task("agent-1", "local_agent", "running", Some(true))),
            );
        });
        let rendered = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value: Context::owned(store)) {
                    TurnDurationMessage(duration: "5s".to_string())
                }
            }
        }
        .render(Some(100))
        .to_string();
        assert!(
            rendered.contains("Worked for 5s · 1 local agent still running"),
            "canvas=\n{rendered}"
        );
    }
}
