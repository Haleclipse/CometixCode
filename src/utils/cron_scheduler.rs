//! Non-React scheduler core for session and durable cron tasks.
//!
//! Maps to: CC `utils/cronScheduler.ts`. REPL drives `on_tick` every 1s;
//! durable tasks are polled from disk and guarded by the scheduler lease.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::utils::cron_tasks::{
    CronJitterConfig, CronTask, DEFAULT_CRON_JITTER_CONFIG, get_scheduled_tasks_enabled,
    jittered_next_cron_run_ms, list_all_cron_tasks, mark_cron_tasks_fired,
    one_shot_jittered_next_cron_run_ms, remove_cron_tasks,
};

pub const CHECK_INTERVAL_MS: u64 = 1000;

/// Maps to: CC `utils/cronScheduler.ts` `isRecurringTaskAged`.
pub fn is_recurring_task_aged(t: &CronTask, now_ms: u64, max_age_ms: u64) -> bool {
    if max_age_ms == 0 {
        return false;
    }
    t.recurring && !t.permanent && now_ms.saturating_sub(t.created_at) >= max_age_ms
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Options for the session cron scheduler.
/// Maps to: CC `CronSchedulerOptions` (session subset).
#[derive(Default)]
pub struct CronSchedulerOptions {
    pub assistant_mode: bool,
    pub get_jitter_config: Option<Box<dyn Fn() -> CronJitterConfig + Send>>,
    pub is_killed: Option<Box<dyn Fn() -> bool + Send>>,
}

/// Maps to: CC `CronScheduler` (tick-driven; no internal timer).
pub struct CronScheduler {
    options: CronSchedulerOptions,
    next_fire_at: HashMap<String, u64>,
    started: bool,
    enable_ready: bool,
    durable_lock: crate::utils::cron_tasks_lock::SchedulerLockGuard,
}

impl CronScheduler {
    /// Maps to: CC `createCronScheduler`.
    pub fn create(options: CronSchedulerOptions) -> Self {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        Self {
            options,
            next_fire_at: HashMap::new(),
            started: false,
            enable_ready: false,
            durable_lock: crate::utils::cron_tasks_lock::SchedulerLockGuard::new(
                &project_root,
                uuid::Uuid::new_v4().to_string(),
            ),
        }
    }

    /// Maps to: CC `CronScheduler.start` (session path — polls enable flag).
    pub fn start(&mut self) {
        self.started = true;
        let durable_enabled = crate::tools::schedule_cron_tool::prompt::is_durable_cron_enabled();
        if durable_enabled && let Err(error) = self.durable_lock.try_acquire() {
            crate::utils::debug::log_for_debugging(&format!(
                "[ScheduledTasks] scheduler lock error: {error}"
            ));
        }
        if get_scheduled_tasks_enabled()
            || (durable_enabled
                && self.durable_lock.is_owned()
                && list_all_cron_tasks()
                    .iter()
                    .any(|task| task.durable != Some(false)))
        {
            self.enable_ready = true;
        }
    }

    /// Maps to: CC `CronScheduler.stop`.
    pub fn stop(&mut self) {
        self.started = false;
        self.enable_ready = false;
        self.next_fire_at.clear();
        self.durable_lock.release();
    }

    /// Epoch ms of the soonest scheduled fire, or None.
    /// Maps to: CC `CronScheduler.getNextFireTime`.
    pub fn get_next_fire_time(&self) -> Option<u64> {
        self.next_fire_at
            .values()
            .copied()
            .filter(|&t| t < u64::MAX)
            .min()
    }

    /// Drive one scheduler tick. Returns tasks that fired this tick.
    /// Maps to: CC `setInterval(check, CHECK_INTERVAL_MS)` body + enable poll.
    pub fn on_tick(&mut self, is_loading: bool) -> Vec<CronTask> {
        if !self.started {
            return Vec::new();
        }
        if self
            .options
            .is_killed
            .as_ref()
            .map(|f| f())
            .unwrap_or(false)
        {
            return Vec::new();
        }
        let durable_enabled = crate::tools::schedule_cron_tool::prompt::is_durable_cron_enabled();
        if durable_enabled
            && !self.durable_lock.is_owned()
            && let Err(error) = self.durable_lock.try_acquire()
        {
            crate::utils::debug::log_for_debugging(&format!(
                "[ScheduledTasks] scheduler lock error: {error}"
            ));
        }
        if !self.enable_ready {
            if get_scheduled_tasks_enabled()
                || (durable_enabled
                    && self.durable_lock.is_owned()
                    && list_all_cron_tasks()
                        .iter()
                        .any(|task| task.durable != Some(false)))
            {
                self.enable_ready = true;
            } else {
                return Vec::new();
            }
        }
        self.check(is_loading)
    }

    fn check(&mut self, is_loading: bool) -> Vec<CronTask> {
        if is_loading && !self.options.assistant_mode {
            return Vec::new();
        }
        let now = now_ms();
        let jitter_cfg = self
            .options
            .get_jitter_config
            .as_ref()
            .map(|f| f())
            .unwrap_or_else(|| DEFAULT_CRON_JITTER_CONFIG);

        let mut fired = Vec::new();
        let mut seen = HashSet::new();
        let durable_enabled = crate::tools::schedule_cron_tool::prompt::is_durable_cron_enabled();
        let tasks = list_all_cron_tasks();

        for t in tasks {
            let durable = t.durable != Some(false);
            if durable && (!durable_enabled || !self.durable_lock.is_owned()) {
                continue;
            }
            seen.insert(t.id.clone());
            let mut next = self.next_fire_at.get(&t.id).copied();
            if next.is_none() {
                let computed = if t.recurring {
                    jittered_next_cron_run_ms(
                        &t.cron,
                        t.last_fired_at.unwrap_or(t.created_at),
                        &t.id,
                        &jitter_cfg,
                    )
                } else {
                    one_shot_jittered_next_cron_run_ms(&t.cron, t.created_at, &t.id, &jitter_cfg)
                }
                .unwrap_or(u64::MAX);
                self.next_fire_at.insert(t.id.clone(), computed);
                next = Some(computed);
                crate::utils::debug::log_for_debugging(&format!(
                    "[ScheduledTasks] scheduled {} for {}",
                    t.id,
                    if computed == u64::MAX {
                        "never".to_string()
                    } else {
                        computed.to_string()
                    }
                ));
            }
            let next = next.unwrap_or(u64::MAX);
            if now < next {
                continue;
            }

            crate::utils::debug::log_for_debugging(&format!(
                "[ScheduledTasks] firing {}{}",
                t.id,
                if t.recurring { " (recurring)" } else { "" }
            ));

            let aged = is_recurring_task_aged(&t, now, jitter_cfg.recurring_max_age_ms);
            fired.push(t.clone());

            if t.recurring && !aged {
                let new_next =
                    jittered_next_cron_run_ms(&t.cron, now, &t.id, &jitter_cfg).unwrap_or(u64::MAX);
                self.next_fire_at.insert(t.id.clone(), new_next);
                if durable
                    && let Err(error) = mark_cron_tasks_fired(std::slice::from_ref(&t.id), now)
                {
                    crate::utils::debug::log_for_debugging(&format!(
                        "[ScheduledTasks] failed to stamp {}: {error}",
                        t.id
                    ));
                }
            } else {
                if let Err(error) = remove_cron_tasks(std::slice::from_ref(&t.id)) {
                    crate::utils::debug::log_for_debugging(&format!(
                        "[ScheduledTasks] failed to remove {}: {error}",
                        t.id
                    ));
                }
                self.next_fire_at.remove(&t.id);
            }
        }

        if seen.is_empty() {
            self.next_fire_at.clear();
            return fired;
        }
        self.next_fire_at.retain(|id, _| seen.contains(id));
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::cron_tasks::{
        add_cron_task, get_session_cron_tasks, reset_cron_tasks_for_test,
        set_scheduled_tasks_enabled,
    };

    #[test]
    fn fires_one_shot_when_due() {
        reset_cron_tasks_for_test();
        // Anchor created_at in the past by manipulating after add — inject directly.
        let id = add_cron_task(
            "* * * * *".to_string(),
            "do the thing".to_string(),
            false,
            false,
            None,
        )
        .unwrap();
        // Force created_at far enough in the past that next fire is due.
        {
            crate::utils::cron_tasks::remove_session_cron_tasks(std::slice::from_ref(&id));
            crate::utils::cron_tasks::add_session_cron_task(CronTask {
                id: id.clone(),
                cron: "* * * * *".to_string(),
                prompt: "do the thing".to_string(),
                created_at: now_ms().saturating_sub(120_000),
                last_fired_at: None,
                recurring: false,
                permanent: false,
                durable: Some(false),
                agent_id: None,
            });
        }
        set_scheduled_tasks_enabled(true);

        let mut scheduler = CronScheduler::create(CronSchedulerOptions {
            assistant_mode: true, // bypass isLoading
            get_jitter_config: Some(Box::new(|| CronJitterConfig {
                // Disable one-shot lead so fire lands on computed mark.
                one_shot_max_ms: 0,
                one_shot_floor_ms: 0,
                one_shot_minute_mod: 30,
                ..DEFAULT_CRON_JITTER_CONFIG
            })),
            is_killed: Some(Box::new(|| false)),
        });
        scheduler.start();
        let fired = scheduler.on_tick(false);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].id, id);
        assert!(get_session_cron_tasks().is_empty());
    }
}
