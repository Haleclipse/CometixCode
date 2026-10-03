//! Task utility seam.
//!
//! Maps to CC `utils/tasks.ts` (TodoV2 disk store under `~/.claude/tasks/`).
//! Local Bash lifecycle state belongs to `tasks/local_shell_task`, matching
//! CC's source ownership.

use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::utils::process_env::JsTruthy;
#[cfg(test)]
use crate::utils::test_env::{EnvVarGuard, TestStateLock};

const HIGH_WATER_MARK_FILE: &str = ".highwatermark";

/// Maps to: CC `utils/tasks.ts:69` `TASK_STATUSES`.
pub const TASK_STATUSES: [&str; 3] = ["pending", "in_progress", "completed"];

/// Maps to: CC `utils/tasks.ts:71-73` `TaskStatusSchema`.
///
/// `TaskUpdateTool` widens this with `.or(z.literal('deleted'))`; the base set
/// stays here, where CC keeps it.
pub fn task_status_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::LazyLock<crate::utils::zod::Schema> =
        std::sync::LazyLock::new(|| crate::utils::zod::enumeration(TASK_STATUSES.to_vec()));
    &SCHEMA
}

static LEADER_TEAM_NAME: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Maps to: CC `utils/tasks.ts#sanitizePathComponent`.
pub fn sanitize_path_component(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Maps to: CC `utils/tasks.ts#setLeaderTeamName`.
pub fn set_leader_team_name(team_name: &str) {
    let mut leader = LEADER_TEAM_NAME.lock().unwrap();
    if leader.as_deref() == Some(team_name) {
        return;
    }
    *leader = Some(team_name.to_string());
}

/// Maps to: CC `utils/tasks.ts#clearLeaderTeamName`.
pub fn clear_leader_team_name() {
    *LEADER_TEAM_NAME.lock().unwrap() = None;
}

/// Maps to: CC `utils/tasks.ts#getTaskListId`.
pub fn get_task_list_id() -> String {
    if let Some(task_list_id) = crate::utils::process_env::var("CLAUDE_CODE_TASK_LIST_ID").truthy()
    {
        return task_list_id;
    }
    crate::utils::teammate::get_team_name(None)
        .or_else(|| LEADER_TEAM_NAME.lock().unwrap().clone())
        .unwrap_or_else(crate::bootstrap::state::get_session_id)
}

/// Maps to: CC `utils/tasks.ts#getTasksDir`.
pub fn get_tasks_dir(task_list_id: &str) -> std::path::PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir()
        .join("tasks")
        .join(sanitize_path_component(task_list_id))
}

/// Maps to: CC `utils/tasks.ts#ensureTasksDir`.
pub fn ensure_tasks_dir(task_list_id: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(get_tasks_dir(task_list_id))
}

/// Maps to: CC `utils/tasks.ts#getHighWaterMarkPath`.
fn high_water_mark_path(task_list_id: &str) -> std::path::PathBuf {
    get_tasks_dir(task_list_id).join(HIGH_WATER_MARK_FILE)
}

fn read_high_water_mark(task_list_id: &str) -> u64 {
    std::fs::read_to_string(high_water_mark_path(task_list_id))
        .ok()
        .and_then(|content| content.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn write_high_water_mark(task_list_id: &str, value: u64) -> std::io::Result<()> {
    ensure_tasks_dir(task_list_id)?;
    std::fs::write(high_water_mark_path(task_list_id), value.to_string())
}

fn find_highest_task_id_from_files(task_list_id: &str) -> u64 {
    let Ok(entries) = std::fs::read_dir(get_tasks_dir(task_list_id)) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".json") && !name.starts_with('.'))
        .filter_map(|name| name.trim_end_matches(".json").parse::<u64>().ok())
        .max()
        .unwrap_or(0)
}

/// Maps to: CC `utils/tasks.ts#findHighestTaskId`.
fn find_highest_task_id(task_list_id: &str) -> u64 {
    find_highest_task_id_from_files(task_list_id).max(read_high_water_mark(task_list_id))
}

/// Maps to: CC `utils/tasks.ts#getTaskPath`.
pub fn get_task_path(task_list_id: &str, task_id: &str) -> std::path::PathBuf {
    get_tasks_dir(task_list_id).join(format!("{}.json", sanitize_path_component(task_id)))
}

/// Process-local serialization for create/update/delete (CC uses proper-lockfile).
static TASK_LIST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Maps to: CC `utils/tasks.ts#resetTaskList`.
pub fn reset_task_list(task_list_id: &str) -> std::io::Result<()> {
    let _guard = TASK_LIST_LOCK.lock().unwrap();
    ensure_tasks_dir(task_list_id)?;
    let current_highest = find_highest_task_id_from_files(task_list_id);
    let existing_mark = read_high_water_mark(task_list_id);
    if current_highest > existing_mark {
        write_high_water_mark(task_list_id, current_highest)?;
    }
    if let Ok(entries) = std::fs::read_dir(get_tasks_dir(task_list_id)) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if name.ends_with(".json") && !name.starts_with('.') {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

/// Maps to: CC `utils/tasks.ts:133-139` `isTodoV2Enabled()`.
///
/// Force-enable tasks in non-interactive mode via `CLAUDE_CODE_ENABLE_TASKS`
/// (SDK users who want Task tools over TodoWrite); otherwise Task tools are
/// interactive-only, keyed off the bootstrap session flag
/// (`getIsNonInteractiveSession()`), not an env var.
pub fn is_todo_v2_enabled() -> bool {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_ENABLE_TASKS").as_deref(),
    ) {
        return true;
    }
    !crate::bootstrap::state::get_is_non_interactive_session()
}

/// A task tracked by the TaskCreate/Get/List/Update tool family.
/// Maps to: CC `utils/tasks.ts` `Task` / `TaskSchema` (disk JSON). Also the
/// `task_reminder` attachment's `content: Task[]` element
/// (utils/attachments.ts:485-489), hence `pub` + `Eq` for the typed
/// [`crate::utils::attachments::Attachment`] union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) description: String,
    /// Maps to CC `Task.activeForm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_form: Option<String>,
    pub(crate) status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) owner: Option<String>,
    #[serde(default)]
    pub(crate) blocks: Vec<String>,
    #[serde(default)]
    pub(crate) blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// New-task payload without id — maps to CC `Omit<Task, 'id'>`.
#[derive(Debug, Clone)]
pub(crate) struct NewTaskData {
    pub(crate) subject: String,
    pub(crate) description: String,
    pub(crate) active_form: Option<String>,
    pub(crate) status: String,
    pub(crate) owner: Option<String>,
    pub(crate) blocks: Vec<String>,
    pub(crate) blocked_by: Vec<String>,
    pub(crate) metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Maps to: CC `utils/tasks.ts#createTask`.
pub(crate) fn create_task(task_list_id: &str, data: NewTaskData) -> std::io::Result<String> {
    let _guard = TASK_LIST_LOCK.lock().unwrap();
    ensure_tasks_dir(task_list_id)?;
    let id = (find_highest_task_id(task_list_id) + 1).to_string();
    let task = TaskRecord {
        id: id.clone(),
        subject: data.subject,
        description: data.description,
        active_form: data.active_form,
        status: data.status,
        owner: data.owner,
        blocks: data.blocks,
        blocked_by: data.blocked_by,
        metadata: data.metadata,
    };
    let path = get_task_path(task_list_id, &id);
    let json = serde_json::to_string_pretty(&task)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)?;
    Ok(id)
}

/// Maps to: CC `utils/tasks.ts#getTask`.
pub(crate) fn get_task(task_list_id: &str, task_id: &str) -> Option<TaskRecord> {
    let path = get_task_path(task_list_id, task_id);
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_task(task_list_id: &str, task: &TaskRecord) -> std::io::Result<()> {
    ensure_tasks_dir(task_list_id)?;
    let path = get_task_path(task_list_id, &task.id);
    let json = serde_json::to_string_pretty(task)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

/// Maps to: CC `utils/tasks.ts#listTasks`.
pub(crate) fn list_tasks(task_list_id: &str) -> Vec<TaskRecord> {
    let Ok(entries) = std::fs::read_dir(get_tasks_dir(task_list_id)) else {
        return Vec::new();
    };
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        let id = name.trim_end_matches(".json");
        if let Some(task) = get_task(task_list_id, id) {
            tasks.push(task);
        }
    }
    tasks.sort_by(|a, b| {
        a.id.parse::<u64>()
            .unwrap_or(0)
            .cmp(&b.id.parse::<u64>().unwrap_or(0))
    });
    tasks
}

/// Maps to: CC `utils/tasks.ts#deleteTask`.
pub(crate) fn delete_task(task_list_id: &str, task_id: &str) -> bool {
    let _guard = TASK_LIST_LOCK.lock().unwrap();
    let path = get_task_path(task_list_id, task_id);
    if let Ok(numeric_id) = task_id.parse::<u64>() {
        let current_mark = read_high_water_mark(task_list_id);
        if numeric_id > current_mark {
            let _ = write_high_water_mark(task_list_id, numeric_id);
        }
    }
    if std::fs::remove_file(&path).is_err() {
        return false;
    }
    let all = list_tasks(task_list_id);
    let mut cleanup_succeeded = true;
    for task in all {
        let new_blocks: Vec<String> = task
            .blocks
            .iter()
            .filter(|id| *id != task_id)
            .cloned()
            .collect();
        let new_blocked_by: Vec<String> = task
            .blocked_by
            .iter()
            .filter(|id| *id != task_id)
            .cloned()
            .collect();
        if new_blocks.len() != task.blocks.len() || new_blocked_by.len() != task.blocked_by.len() {
            let mut updated = task;
            updated.blocks = new_blocks;
            updated.blocked_by = new_blocked_by;
            if write_task(task_list_id, &updated).is_err() {
                cleanup_succeeded = false;
            }
        }
    }
    cleanup_succeeded
}

/// Maps to: CC `utils/tasks.ts#blockTask`.
pub(crate) fn block_task(task_list_id: &str, from_task_id: &str, to_task_id: &str) -> bool {
    let _guard = TASK_LIST_LOCK.lock().unwrap();
    let Some(mut from_task) = get_task(task_list_id, from_task_id) else {
        return false;
    };
    let Some(mut to_task) = get_task(task_list_id, to_task_id) else {
        return false;
    };
    if !from_task.blocks.iter().any(|id| id == to_task_id) {
        from_task.blocks.push(to_task_id.to_string());
        if write_task(task_list_id, &from_task).is_err() {
            return false;
        }
    }
    if !to_task.blocked_by.iter().any(|id| id == from_task_id) {
        to_task.blocked_by.push(from_task_id.to_string());
        if write_task(task_list_id, &to_task).is_err() {
            return false;
        }
    }
    true
}

/// Maps to: CC `utils/tasks.ts#ClaimTaskResult`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClaimTaskResult {
    pub(crate) success: bool,
    pub(crate) reason: Option<&'static str>,
    pub(crate) task: Option<TaskRecord>,
    pub(crate) busy_with_tasks: Vec<String>,
    pub(crate) blocked_by_tasks: Vec<String>,
}

impl ClaimTaskResult {
    fn success(task: TaskRecord) -> Self {
        Self {
            success: true,
            reason: None,
            task: Some(task),
            busy_with_tasks: Vec::new(),
            blocked_by_tasks: Vec::new(),
        }
    }

    fn failure(reason: &'static str, task: Option<TaskRecord>) -> Self {
        Self {
            success: false,
            reason: Some(reason),
            task,
            busy_with_tasks: Vec::new(),
            blocked_by_tasks: Vec::new(),
        }
    }
}

/// Maps to: CC `utils/tasks.ts#claimTask` (process-local lock; no file lock).
pub(crate) fn claim_task(
    task_list_id: &str,
    task_id: &str,
    claimant_agent_id: &str,
) -> ClaimTaskResult {
    let _guard = TASK_LIST_LOCK.lock().unwrap();
    let Some(task) = get_task(task_list_id, task_id) else {
        return ClaimTaskResult::failure("task_not_found", None);
    };
    if task
        .owner
        .as_deref()
        .is_some_and(|owner| owner != claimant_agent_id)
    {
        return ClaimTaskResult::failure("already_claimed", Some(task));
    }
    if task.status == "completed" {
        return ClaimTaskResult::failure("already_resolved", Some(task));
    }

    let unresolved_task_ids = list_tasks(task_list_id)
        .into_iter()
        .filter(|task| task.status != "completed")
        .map(|task| task.id)
        .collect::<std::collections::HashSet<_>>();
    let blocked_by_tasks = task
        .blocked_by
        .iter()
        .filter(|id| unresolved_task_ids.contains(*id))
        .cloned()
        .collect::<Vec<_>>();
    if !blocked_by_tasks.is_empty() {
        let mut result = ClaimTaskResult::failure("blocked", Some(task));
        result.blocked_by_tasks = blocked_by_tasks;
        return result;
    }

    let mut claimed = task;
    claimed.owner = Some(claimant_agent_id.to_string());
    if write_task(task_list_id, &claimed).is_err() {
        return ClaimTaskResult::failure("task_not_found", None);
    }
    ClaimTaskResult::success(claimed)
}

/// Patch shape for `update_task`; maps to TS `Partial<Omit<Task, 'id'>>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TaskUpdatePatch {
    pub(crate) subject: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) active_form: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) owner: Option<Option<String>>,
    pub(crate) blocks: Option<Vec<String>>,
    pub(crate) blocked_by: Option<Vec<String>>,
    pub(crate) metadata: Option<Option<serde_json::Map<String, serde_json::Value>>>,
}

/// Maps to: CC `utils/tasks.ts#updateTask`.
pub(crate) fn update_task(
    task_list_id: &str,
    task_id: &str,
    patch: TaskUpdatePatch,
) -> Option<TaskRecord> {
    let _guard = TASK_LIST_LOCK.lock().unwrap();
    let mut task = get_task(task_list_id, task_id)?;
    if let Some(subject) = patch.subject {
        task.subject = subject;
    }
    if let Some(description) = patch.description {
        task.description = description;
    }
    if let Some(active_form) = patch.active_form {
        task.active_form = (!active_form.trim().is_empty()).then_some(active_form);
    }
    if let Some(status) = patch.status {
        task.status = status;
    }
    if let Some(owner) = patch.owner {
        task.owner = owner;
    }
    if let Some(blocks) = patch.blocks {
        task.blocks = blocks;
    }
    if let Some(blocked_by) = patch.blocked_by {
        task.blocked_by = blocked_by;
    }
    if let Some(metadata) = patch.metadata {
        task.metadata = metadata;
    }
    write_task(task_list_id, &task).ok()?;
    Some(task)
}

/// Test/compat helper: clear the current session task list on disk.
/// Replaces the old in-memory `TASK_TOOL_STORE.clear()`.
#[cfg(test)]
pub(crate) fn clear_task_tool_store_for_test() {
    let _ = reset_task_list(&get_task_list_id());
}

/// Isolates TodoV2 disk IO under a temp `CLAUDE_CONFIG_DIR` for tests.
#[cfg(test)]
pub(crate) struct TempTaskConfig {
    pub root: std::path::PathBuf,
    _guards: Vec<EnvVarGuard>,
    /// Redirecting `CLAUDE_CONFIG_DIR` also redirects `~/.claude.json`
    /// (`utils/config.rs:119-120`), so the temp root below carries no
    /// `hasTrustDialogAccepted` for any path and the workspace reads as
    /// UNTRUSTED. CC skips every hook in an untrusted interactive session
    /// (`utils/hooks.ts:1994-1999` over `computeTrustDialogAccepted`'s `false`
    /// default, `utils/config.ts:705-743`), which this port honours in
    /// `services::hooks::should_skip_hook_execution`.
    ///
    /// That is a statement about TASK STORAGE isolation accidentally becoming a
    /// statement about workspace trust. Holding trust here keeps the helper's
    /// meaning to the one it advertises; a task test that means to assert on the
    /// untrusted branch has to say so itself.
    _trust: crate::services::hooks::test_support::SessionTrustGuard,
}

#[cfg(test)]
impl TempTaskConfig {
    pub fn new(list_id: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("cometix-tasks-{}", uuid::Uuid::new_v4().simple()));
        let guards = vec![
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root),
            EnvVarGuard::set("CLAUDE_CODE_TASK_LIST_ID", list_id),
        ];
        let _ = reset_task_list(list_id);
        Self {
            root,
            _guards: guards,
            _trust: crate::services::hooks::test_support::SessionTrustGuard::accepted(),
        }
    }
}

#[cfg(test)]
impl Drop for TempTaskConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Deprecated name kept for call-site migration; clears disk task list.
#[cfg(test)]
pub(crate) static TASK_TOOL_STORE: LazyLock<TaskToolStoreCompat> =
    LazyLock::new(TaskToolStoreCompat::default);

#[cfg(test)]
#[derive(Default)]
pub(crate) struct TaskToolStoreCompat;

#[cfg(test)]
impl TaskToolStoreCompat {
    pub(crate) fn lock(
        &self,
    ) -> Result<
        std::sync::MutexGuard<'_, TaskToolStoreCompatInner>,
        std::sync::PoisonError<std::sync::MutexGuard<'_, TaskToolStoreCompatInner>>,
    > {
        TASK_TOOL_STORE_INNER.lock()
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct TaskToolStoreCompatInner;

#[cfg(test)]
static TASK_TOOL_STORE_INNER: LazyLock<Mutex<TaskToolStoreCompatInner>> =
    LazyLock::new(|| Mutex::new(TaskToolStoreCompatInner));

#[cfg(test)]
impl TaskToolStoreCompatInner {
    pub(crate) fn clear(&mut self) {
        clear_task_tool_store_for_test();
    }

    pub(crate) fn push(&mut self, task: TaskRecord) {
        let list_id = get_task_list_id();
        let _ = ensure_tasks_dir(&list_id);
        let _ = write_task(&list_id, &task);
        if let Ok(numeric) = task.id.parse::<u64>() {
            let mark = read_high_water_mark(&list_id);
            if numeric > mark {
                let _ = write_high_water_mark(&list_id, numeric);
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        list_tasks(&get_task_list_id()).is_empty()
    }
}

#[cfg(test)]
pub(crate) static TASK_TOOL_TEST_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn task_claim_respects_official_owner_status_and_blocker_rules() {
        let _guard = TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-task-claim-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _list_guard = EnvVarGuard::set("CLAUDE_CODE_TASK_LIST_ID", "session");
        let _ = reset_task_list("session");
        create_task(
            "session",
            NewTaskData {
                subject: "Foundation".to_string(),
                description: String::new(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: None,
            },
        )
        .unwrap();
        create_task(
            "session",
            NewTaskData {
                subject: "Follow-up".to_string(),
                description: String::new(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: vec!["1".to_string()],
                metadata: None,
            },
        )
        .unwrap();

        let blocked = claim_task("session", "2", "worker");
        assert!(!blocked.success);
        assert_eq!(blocked.reason, Some("blocked"));
        assert_eq!(blocked.blocked_by_tasks, vec!["1".to_string()]);

        update_task(
            "session",
            "1",
            TaskUpdatePatch {
                status: Some("completed".to_string()),
                ..Default::default()
            },
        );
        let claimed = claim_task("session", "2", "worker");
        assert!(claimed.success);
        assert_eq!(
            claimed.task.as_ref().unwrap().owner.as_deref(),
            Some("worker")
        );

        let already_claimed = claim_task("session", "2", "other");
        assert!(!already_claimed.success);
        assert_eq!(already_claimed.reason, Some("already_claimed"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn leader_team_name_and_task_list_id_follow_official_priority() {
        let _guard = TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _teammate_guard = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _task_list_guard = EnvVarGuard::set("CLAUDE_CODE_TASK_LIST_ID", "");
        clear_leader_team_name();
        crate::utils::teammate::clear_dynamic_team_context();

        set_leader_team_name("alpha-team");
        assert_eq!(get_task_list_id(), "alpha-team");

        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "reviewer@pane-team".to_string(),
                agent_name: "reviewer".to_string(),
                team_name: "pane-team".to_string(),
                color: None,
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));
        assert_eq!(get_task_list_id(), "pane-team");

        crate::utils::process_env::set("CLAUDE_CODE_TASK_LIST_ID", "explicit-list");
        assert_eq!(get_task_list_id(), "explicit-list");

        crate::utils::process_env::set("CLAUDE_CODE_TASK_LIST_ID", "");
        crate::utils::teammate::clear_dynamic_team_context();
        clear_leader_team_name();
    }

    #[test]
    fn reset_task_list_removes_task_files_and_preserves_high_water_mark() {
        let _guard = TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-task-reset-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let task_list_id = "alpha";
        let dir = get_tasks_dir(task_list_id);
        ensure_tasks_dir(task_list_id).unwrap();
        std::fs::write(dir.join("1.json"), "{}").unwrap();
        std::fs::write(dir.join("9.json"), "{}").unwrap();
        std::fs::write(dir.join(".ignored.json"), "{}").unwrap();
        TASK_TOOL_STORE.lock().unwrap().push(TaskRecord {
            id: "1".to_string(),
            subject: "Old task".to_string(),
            description: String::new(),
            active_form: None,
            status: "pending".to_string(),
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: None,
        });

        reset_task_list(task_list_id).unwrap();

        assert!(!dir.join("1.json").exists());
        assert!(!dir.join("9.json").exists());
        assert!(dir.join(".ignored.json").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(HIGH_WATER_MARK_FILE)).unwrap(),
            "9"
        );
        assert!(list_tasks(task_list_id).is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn is_todo_v2_enabled_matches_official_interactive_default_and_env_override() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_ENABLE_TASKS");
        crate::utils::process_env::remove("COMETIX_NON_INTERACTIVE_SESSION");
        assert!(super::is_todo_v2_enabled());

        crate::utils::process_env::set("COMETIX_NON_INTERACTIVE_SESSION", "1");
        assert!(!super::is_todo_v2_enabled());

        crate::utils::process_env::set("CLAUDE_CODE_ENABLE_TASKS", "1");
        assert!(super::is_todo_v2_enabled());
        crate::utils::process_env::remove("CLAUDE_CODE_ENABLE_TASKS");
        crate::utils::process_env::remove("COMETIX_NON_INTERACTIVE_SESSION");
    }
}
