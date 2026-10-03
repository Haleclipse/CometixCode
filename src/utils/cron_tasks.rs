//! Scheduled cron task store for the CronCreate/CronDelete/CronList tools.
//!
//! Maps to: CC `utils/cronTasks.ts` (`addCronTask`, `removeCronTasks`,
//! `listAllCronTasks`, `nextCronRunMs`, jitter helpers).
//!
//! Session and file-backed `.claude/scheduled_tasks.json` paths are live.

use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone, Timelike};

use crate::utils::cron::{compute_next_cron_run, parse_cron_expression};

/// Maps to: CC `utils/cronTasks.ts` `CronTask`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronTask {
    pub id: String,
    pub cron: String,
    pub prompt: String,
    /// Epoch ms when the task was created.
    pub created_at: u64,
    /// Epoch ms of the most recent fire (recurring only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub recurring: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub permanent: bool,
    /// false → session-scoped; true/None → file-backed (durable path).
    #[serde(skip)]
    pub durable: Option<bool>,
    /// Runtime-only. Set when an in-process teammate created the task; the
    /// durable path never attaches it because teammate crons are session-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// Maps to: CC `utils/cronTasks.ts` `CronJitterConfig`.
#[derive(Clone, Copy, Debug)]
pub struct CronJitterConfig {
    pub recurring_frac: f64,
    pub recurring_cap_ms: u64,
    pub one_shot_max_ms: u64,
    pub one_shot_floor_ms: u64,
    pub one_shot_minute_mod: u32,
    pub recurring_max_age_ms: u64,
}

/// Maps to: CC `DEFAULT_CRON_JITTER_CONFIG`.
pub const DEFAULT_CRON_JITTER_CONFIG: CronJitterConfig = CronJitterConfig {
    recurring_frac: 0.1,
    recurring_cap_ms: 15 * 60 * 1000,
    one_shot_max_ms: 90 * 1000,
    one_shot_floor_ms: 0,
    one_shot_minute_mod: 30,
    recurring_max_age_ms: 7 * 24 * 60 * 60 * 1000,
};

static SESSION_CRON_TASKS: LazyLock<Mutex<Vec<CronTask>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static SCHEDULED_TASKS_ENABLED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static DURABLE_CRON_MUTATION_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Serialize, Deserialize)]
struct CronFile {
    tasks: Vec<CronTask>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Maps to: CC `bootstrap/state.ts` `getScheduledTasksEnabled`.
pub fn get_scheduled_tasks_enabled() -> bool {
    *SCHEDULED_TASKS_ENABLED.lock().unwrap()
}

/// Maps to: CC `bootstrap/state.ts` `setScheduledTasksEnabled`.
pub fn set_scheduled_tasks_enabled(enabled: bool) {
    *SCHEDULED_TASKS_ENABLED.lock().unwrap() = enabled;
}

/// Maps to: CC `bootstrap/state.ts` `getSessionCronTasks`.
pub fn get_session_cron_tasks() -> Vec<CronTask> {
    SESSION_CRON_TASKS.lock().unwrap().clone()
}

/// Maps to: CC `bootstrap/state.ts` `addSessionCronTask`.
pub fn add_session_cron_task(task: CronTask) {
    SESSION_CRON_TASKS.lock().unwrap().push(task);
}

/// Maps to: CC `bootstrap/state.ts` `removeSessionCronTasks`.
/// Returns the number of tasks removed.
pub fn remove_session_cron_tasks(ids: &[String]) -> usize {
    if ids.is_empty() {
        return 0;
    }
    let id_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut tasks = SESSION_CRON_TASKS.lock().unwrap();
    let before = tasks.len();
    tasks.retain(|t| !id_set.contains(t.id.as_str()));
    before - tasks.len()
}

/// Maps to: CC `cronTasks.ts:81-83` `getCronFilePath(dir?)` —
/// `join(dir ?? getProjectRoot(), CRON_FILE_REL)`. The `dir` parameter is
/// CC's test-injection seam; Rust tests pin the cwd instead.
pub(crate) fn get_cron_file_path() -> std::path::PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".claude")
        .join("scheduled_tasks.json")
}

fn read_durable_cron_tasks() -> Vec<CronTask> {
    let Ok(raw) = std::fs::read_to_string(get_cron_file_path()) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_str::<CronFile>(&raw) else {
        return Vec::new();
    };
    file.tasks
        .into_iter()
        .filter(|task| parse_cron_expression(&task.cron).is_some())
        .map(|mut task| {
            task.durable = None;
            task
        })
        .collect()
}

fn write_durable_cron_tasks(tasks: &[CronTask]) -> Result<(), String> {
    if !crate::utils::session_storage::is_session_write_enabled() {
        return Err(crate::tools::shared::write_gate::CRON_PERSISTENCE_DISABLED_ERROR.to_string());
    }
    let path = get_cron_file_path();
    let dir = path
        .parent()
        .ok_or_else(|| "Invalid scheduled task path".to_string())?;
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("Failed to create {}: {error}", dir.display()))?;
    let body = serde_json::to_string_pretty(&CronFile {
        tasks: tasks
            .iter()
            .cloned()
            .map(|mut task| {
                task.durable = None;
                task
            })
            .collect(),
    })
    .map_err(|error| format!("Failed to serialize scheduled tasks: {error}"))?;
    let temporary = dir.join(format!(
        ".scheduled_tasks.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&temporary, format!("{body}\n"))
        .map_err(|error| format!("Failed to write {}: {error}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Failed to replace {}: {error}", path.display()));
    }
    Ok(())
}

/// Maps to: CC `utils/cronTasks.ts` `addCronTask`.
pub fn add_cron_task(
    cron: String,
    prompt: String,
    recurring: bool,
    durable: bool,
    agent_id: Option<String>,
) -> Result<String, String> {
    let full_id = uuid::Uuid::new_v4().to_string();
    let id = full_id[..8].to_string();
    let mut task = CronTask {
        id: id.clone(),
        cron,
        prompt,
        created_at: now_ms(),
        last_fired_at: None,
        recurring,
        permanent: false,
        durable: if durable { None } else { Some(false) },
        agent_id: None,
    };
    if !durable {
        // Official attaches `agentId` only on the session branch; durable
        // teammate crons are rejected upstream by `CronCreateTool.validateInput`.
        task.agent_id = agent_id;
        add_session_cron_task(task);
        return Ok(id);
    }
    let _guard = DURABLE_CRON_MUTATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut tasks = read_durable_cron_tasks();
    tasks.push(task);
    write_durable_cron_tasks(&tasks)?;
    Ok(id)
}

/// Maps to: CC `utils/cronTasks.ts` `removeCronTasks`.
pub fn remove_cron_tasks(ids: &[String]) -> Result<usize, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    let removed_session = remove_session_cron_tasks(ids);
    if removed_session == ids.len() {
        return Ok(removed_session);
    }
    let _guard = DURABLE_CRON_MUTATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let id_set = ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let tasks = read_durable_cron_tasks();
    let before = tasks.len();
    let remaining = tasks
        .into_iter()
        .filter(|task| !id_set.contains(task.id.as_str()))
        .collect::<Vec<_>>();
    let removed_durable = before - remaining.len();
    if removed_durable > 0 {
        write_durable_cron_tasks(&remaining)?;
    }
    Ok(removed_session + removed_durable)
}

/// Maps to: CC `utils/cronTasks.ts` `markCronTasksFired`.
pub fn mark_cron_tasks_fired(ids: &[String], fired_at: u64) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let _guard = DURABLE_CRON_MUTATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let id_set = ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut tasks = read_durable_cron_tasks();
    let mut changed = false;
    for task in &mut tasks {
        if id_set.contains(task.id.as_str()) {
            task.last_fired_at = Some(fired_at);
            changed = true;
        }
    }
    if changed {
        write_durable_cron_tasks(&tasks)?;
    }
    Ok(())
}

/// Maps to: CC `utils/cronTasks.ts` `listAllCronTasks`.
pub fn list_all_cron_tasks() -> Vec<CronTask> {
    let mut tasks = read_durable_cron_tasks();
    tasks.extend(get_session_cron_tasks().into_iter().map(|mut task| {
        task.durable = Some(false);
        task
    }));
    tasks
}

/// Next fire time in epoch ms for a cron string, strictly after `from_ms`.
/// Maps to: CC `utils/cronTasks.ts` `nextCronRunMs`.
pub fn next_cron_run_ms(cron: &str, from_ms: u64) -> Option<u64> {
    let fields = parse_cron_expression(cron)?;
    let from = Local.timestamp_millis_opt(from_ms as i64).single()?;
    let next = compute_next_cron_run(&fields, from)?;
    Some(next.timestamp_millis() as u64)
}

fn jitter_frac(task_id: &str) -> f64 {
    let hex = &task_id[..task_id.len().min(8)];
    let parsed = u64::from_str_radix(hex, 16).ok();
    match parsed {
        Some(n) => (n as f64) / (0x1_0000_0000u64 as f64),
        None => 0.0,
    }
}

/// Maps to: CC `utils/cronTasks.ts` `jitteredNextCronRunMs`.
pub fn jittered_next_cron_run_ms(
    cron: &str,
    from_ms: u64,
    task_id: &str,
    cfg: &CronJitterConfig,
) -> Option<u64> {
    let t1 = next_cron_run_ms(cron, from_ms)?;
    let Some(t2) = next_cron_run_ms(cron, t1) else {
        return Some(t1);
    };
    let jitter = ((jitter_frac(task_id) * cfg.recurring_frac * (t2 - t1) as f64) as u64)
        .min(cfg.recurring_cap_ms);
    Some(t1 + jitter)
}

/// Maps to: CC `utils/cronTasks.ts` `oneShotJitteredNextCronRunMs`.
pub fn one_shot_jittered_next_cron_run_ms(
    cron: &str,
    from_ms: u64,
    task_id: &str,
    cfg: &CronJitterConfig,
) -> Option<u64> {
    let t1 = next_cron_run_ms(cron, from_ms)?;
    let minute = Local
        .timestamp_millis_opt(t1 as i64)
        .single()
        .map(|d| d.minute())
        .unwrap_or(0);
    if cfg.one_shot_minute_mod == 0 || minute % cfg.one_shot_minute_mod != 0 {
        return Some(t1);
    }
    let lead = cfg.one_shot_floor_ms
        + (jitter_frac(task_id) * (cfg.one_shot_max_ms - cfg.one_shot_floor_ms) as f64) as u64;
    Some(t1.saturating_sub(lead).max(from_ms))
}

/// Test helper — clears session store + enable flag.
#[cfg(test)]
pub fn reset_cron_tasks_for_test() {
    SESSION_CRON_TASKS.lock().unwrap().clear();
    *SCHEDULED_TASKS_ENABLED.lock().unwrap() = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn add_and_list_session_task() {
        reset_cron_tasks_for_test();
        let id = add_cron_task(
            "*/5 * * * *".to_string(),
            "ping".to_string(),
            true,
            false,
            None,
        )
        .unwrap();
        set_scheduled_tasks_enabled(true);
        let tasks = list_all_cron_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id);
        assert_eq!(tasks[0].durable, Some(false));
        assert!(get_scheduled_tasks_enabled());
        remove_cron_tasks(&[id]).unwrap();
        assert!(list_all_cron_tasks().is_empty());
    }

    #[test]
    fn durable_tasks_round_trip_through_project_file() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        struct Restore {
            cwd: std::path::PathBuf,
            write: Option<EnvVarGuard>,
            root: std::path::PathBuf,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.cwd);
                drop(self.write.take());
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
        let root = std::env::temp_dir().join(format!(
            "cometix-durable-cron-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let restore = Restore {
            cwd: std::env::current_dir().unwrap(),
            write: Some(EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1")),
            root: root.clone(),
        };
        std::env::set_current_dir(&root).unwrap();

        let id = add_cron_task(
            "*/5 * * * *".to_string(),
            "durable ping".to_string(),
            true,
            true,
            None,
        )
        .unwrap();
        let path = root.join(".claude/scheduled_tasks.json");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains(&id));
        assert!(!raw.contains("\"durable\""));
        assert_eq!(list_all_cron_tasks()[0].durable, None);
        assert_eq!(remove_cron_tasks(&[id]).unwrap(), 1);
        assert!(list_all_cron_tasks().is_empty());
        drop(restore);
    }

    #[test]
    fn next_cron_run_ms_every_minute() {
        let from = 1_721_000_000_000u64; // fixed epoch
        let next = next_cron_run_ms("* * * * *", from).expect("match");
        assert!(next > from);
        assert!(next - from <= 60_000);
    }
}
