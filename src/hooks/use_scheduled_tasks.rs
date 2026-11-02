//! REPL wrapper for the cron scheduler.
//!
//! Maps to: CC `hooks/useScheduledTasks.ts`.
//! Fired prompts go into the command queue as `later` priority; REPL drains
//! them between turns (same role as `useQueueProcessor`).

use chrono::{Datelike, Local, Timelike};

use crate::tools::schedule_cron_tool::prompt::is_kairos_cron_enabled;
use crate::utils::cron_scheduler::{CHECK_INTERVAL_MS, CronScheduler, CronSchedulerOptions};
use crate::utils::cron_tasks::{CronTask, DEFAULT_CRON_JITTER_CONFIG};
use crate::utils::message_queue_manager::{QueuedCommand, enqueue_pending_notification};

pub use crate::utils::cron_scheduler::CHECK_INTERVAL_MS as SCHEDULED_TASKS_INTERVAL_MS;

/// Maps to: CC `useScheduledTasks.ts` `formatCronFireTime`.
pub fn format_cron_fire_time(d: chrono::DateTime<Local>) -> String {
    // en-US-ish: "Jul 14 4:55pm"
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = months[(d.month0() as usize).min(11)];
    let day = d.day();
    let hour = d.hour();
    let minute = d.minute();
    let (h12, ampm) = match hour {
        0 => (12, "am"),
        1..=11 => (hour, "am"),
        12 => (12, "pm"),
        _ => (hour - 12, "pm"),
    };
    format!("{month} {day} {h12}:{minute:02}{ampm}")
}

/// System-message content for a normal fire.
/// Maps to: CC `createScheduledTaskFireMessage` content string.
pub fn scheduled_task_fire_content() -> String {
    format!(
        "Running scheduled task ({})",
        format_cron_fire_time(Local::now())
    )
}

/// Enqueue a fired cron prompt for the lead REPL.
/// Maps to: CC `useScheduledTasks.ts` `enqueueForLead` (:72-76). CC also
/// stamps `workload` for the billing-header attribution block — the Rust
/// `QueuedCommand` has no workload field yet (billing-attribution seam).
pub fn enqueue_cron_fire_prompt(prompt: &str) {
    enqueue_pending_notification(QueuedCommand {
        value: prompt.to_string(),
        pre_expansion_value: None,
        pasted_contents: Default::default(),
        mode: "prompt".to_string(),
        priority: crate::utils::message_queue_manager::QueuePriority::Later,
        agent_id: None,
        is_meta: true,
        uuid: None,
        skip_slash_commands: false,
    });
}

/// Create the REPL cron scheduler (session-only).
/// Maps to: CC `useScheduledTasks` effect body constructing `createCronScheduler`.
pub fn create_repl_cron_scheduler(assistant_mode: bool) -> CronScheduler {
    CronScheduler::create(CronSchedulerOptions {
        assistant_mode,
        get_jitter_config: Some(Box::new(|| DEFAULT_CRON_JITTER_CONFIG)),
        is_killed: Some(Box::new(|| !is_kairos_cron_enabled())),
    })
}

/// Handle a fired task the way REPL `onFireTask` does (no teammate routing).
/// Returns the system-message text to append, or None when the task was dropped.
pub fn handle_fired_task_for_lead(task: &CronTask) -> Option<String> {
    if task.agent_id.is_some() {
        // Teammate routing is deferred; drop orphaned teammate crons.
        if let Err(error) =
            crate::utils::cron_tasks::remove_cron_tasks(std::slice::from_ref(&task.id))
        {
            crate::utils::debug::log_for_debugging(&format!(
                "[ScheduledTasks] failed to remove orphaned cron {}: {error}",
                task.id
            ));
        }
        crate::utils::debug::log_for_debugging(&format!(
            "[ScheduledTasks] teammate {} gone / unported, removing cron {}",
            task.agent_id.as_deref().unwrap_or(""),
            task.id
        ));
        return None;
    }
    let content = scheduled_task_fire_content();
    enqueue_cron_fire_prompt(&task.prompt);
    Some(content)
}

/// True when the kairos cron runtime should tick.
pub fn should_run_scheduled_tasks() -> bool {
    is_kairos_cron_enabled()
}

pub const fn check_interval_ms() -> u64 {
    CHECK_INTERVAL_MS
}
