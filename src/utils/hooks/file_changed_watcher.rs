//! FileChanged/CwdChanged hook watcher state.
//! Maps to: CC `utils/hooks/fileChangedWatcher.ts`.
//!
//! CC uses chokidar to subscribe to matcher-derived files and dynamic
//! `watchPaths` returned by env hooks. Cometix has not added a cross-platform
//! file watcher dependency yet, so this module ports the official state machine
//! (path resolution, dynamic watch updates, cwd-change hook dispatch, notifier
//! behavior) and exposes `handle_file_event_with_config` for the future watcher
//! backend to call. No background filesystem subscription is started here.

use super::hooks_config_snapshot::get_hooks_config_from_snapshot;
use crate::services::hooks::env::{
    EnvHookExecutionResult, HookOutsideReplResult, TOOL_HOOK_EXECUTION_TIMEOUT_MS,
    execute_cwd_changed_hooks_with_config, execute_file_changed_hooks_with_config,
};
use crate::services::hooks::{HookEvent, RegisteredHooks};

/// Fold the settings-sourced snapshot into the execution-facing table (the
/// same `from_config_entry` merge the loading chain performs).
fn registered_snapshot() -> Option<RegisteredHooks> {
    get_hooks_config_from_snapshot().map(|config| {
        config
            .iter()
            .map(|(event, entries)| {
                (
                    event.clone(),
                    entries
                        .iter()
                        .map(crate::schemas::hooks::RegisteredHookMatcher::from_config_entry)
                        .collect(),
                )
            })
            .collect()
    })
}
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

type EnvHookNotifier = Arc<dyn Fn(String, bool) + Send + Sync + 'static>;

#[derive(Default)]
struct FileChangedWatcherState {
    current_cwd: String,
    dynamic_watch_paths: Vec<String>,
    dynamic_watch_paths_sorted: Vec<String>,
    watched_paths: Vec<String>,
    initialized: bool,
    has_env_hooks: bool,
    notify_callback: Option<EnvHookNotifier>,
}

/// Test/debug snapshot of the watcher state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileChangedWatcherSnapshot {
    pub current_cwd: String,
    pub dynamic_watch_paths: Vec<String>,
    pub watched_paths: Vec<String>,
    pub initialized: bool,
    pub has_env_hooks: bool,
}

static FILE_CHANGED_WATCHER_STATE: LazyLock<Mutex<FileChangedWatcherState>> =
    LazyLock::new(|| Mutex::new(FileChangedWatcherState::default()));

fn has_env_hooks(config: Option<&RegisteredHooks>) -> bool {
    config.is_some_and(|config| {
        config
            .get(HookEvent::CwdChanged.as_str())
            .is_some_and(|entries| !entries.is_empty())
            || config
                .get(HookEvent::FileChanged.as_str())
                .is_some_and(|entries| !entries.is_empty())
    })
}

fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().to_string()
}

fn push_unique(paths: &mut Vec<String>, path: String) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

/// Maps to: CC local `resolveWatchPaths(config)`.
pub fn resolve_watch_paths(
    config: Option<&RegisteredHooks>,
    current_cwd: &str,
    dynamic_watch_paths: &[String],
) -> Vec<String> {
    let mut paths = Vec::new();
    let cwd = Path::new(current_cwd);

    let matchers = config
        .and_then(|config| config.get(HookEvent::FileChanged.as_str()))
        .cloned()
        .unwrap_or_default();

    for matcher in matchers {
        let Some(pattern) = matcher.matcher else {
            continue;
        };
        for name in pattern
            .split('|')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let path = Path::new(name);
            let resolved = if path.is_absolute() {
                name.to_string()
            } else {
                path_to_string(cwd.join(path))
            };
            push_unique(&mut paths, resolved);
        }
    }

    for path in dynamic_watch_paths {
        push_unique(&mut paths, path.clone());
    }

    paths
}

fn restart_watching_locked(state: &mut FileChangedWatcherState, config: Option<&RegisteredHooks>) {
    state.watched_paths =
        resolve_watch_paths(config, &state.current_cwd, &state.dynamic_watch_paths);
}

fn notify(text: String, is_error: bool) {
    let callback = FILE_CHANGED_WATCHER_STATE
        .lock()
        .expect("file changed watcher poisoned")
        .notify_callback
        .clone();
    if let Some(callback) = callback {
        callback(text, is_error);
    }
}

fn notify_results(result: &EnvHookExecutionResult) {
    for message in &result.system_messages {
        notify(message.clone(), false);
    }
    for hook_result in &result.results {
        if !hook_result.succeeded && !hook_result.output.is_empty() {
            notify(hook_result.output.clone(), true);
        }
    }
}

/// Maps to: CC `setEnvHookNotifier(cb)`.
pub fn set_env_hook_notifier(callback: Option<EnvHookNotifier>) {
    FILE_CHANGED_WATCHER_STATE
        .lock()
        .expect("file changed watcher poisoned")
        .notify_callback = callback;
}

/// Testable form of CC `initializeFileChangedWatcher(cwd)`.
pub fn initialize_file_changed_watcher_with_config(cwd: &str, config: Option<&RegisteredHooks>) {
    let mut state = FILE_CHANGED_WATCHER_STATE
        .lock()
        .expect("file changed watcher poisoned");
    if state.initialized {
        return;
    }
    state.initialized = true;
    state.current_cwd = cwd.to_string();
    state.has_env_hooks = has_env_hooks(config);
    state.watched_paths =
        resolve_watch_paths(config, &state.current_cwd, &state.dynamic_watch_paths);
}

/// Maps to: CC `initializeFileChangedWatcher(cwd)`.
pub fn initialize_file_changed_watcher(cwd: &str) {
    let snapshot = registered_snapshot();
    initialize_file_changed_watcher_with_config(cwd, snapshot.as_ref());
}

/// Maps to: CC `updateWatchPaths(paths)`.
pub fn update_watch_paths_with_config(paths: Vec<String>, config: Option<&RegisteredHooks>) {
    let mut state = FILE_CHANGED_WATCHER_STATE
        .lock()
        .expect("file changed watcher poisoned");
    if !state.initialized {
        return;
    }
    let mut sorted = paths.clone();
    sorted.sort();
    if sorted == state.dynamic_watch_paths_sorted {
        return;
    }
    state.dynamic_watch_paths = paths;
    state.dynamic_watch_paths_sorted = sorted;
    restart_watching_locked(&mut state, config);
}

/// Maps to: CC `updateWatchPaths(paths)` using the current hook snapshot.
pub fn update_watch_paths(paths: Vec<String>) {
    let snapshot = registered_snapshot();
    update_watch_paths_with_config(paths, snapshot.as_ref());
}

/// Maps to: CC `handleFileEvent(path, event)` body after chokidar receives an event.
pub async fn handle_file_event_with_config(
    path: &str,
    event: &str,
    config: &RegisteredHooks,
    base_input: Value,
    base_env: Vec<(String, String)>,
) -> EnvHookExecutionResult {
    let result = execute_file_changed_hooks_with_config(
        config,
        path,
        event,
        base_input,
        base_env,
        TOOL_HOOK_EXECUTION_TIMEOUT_MS,
    )
    .await;
    if !result.watch_paths.is_empty() {
        update_watch_paths_with_config(result.watch_paths.clone(), Some(config));
    }
    notify_results(&result);
    result
}

/// Maps to: CC `onCwdChangedForHooks(oldCwd, newCwd)` with explicit config.
pub async fn on_cwd_changed_for_hooks_with_config(
    old_cwd: &str,
    new_cwd: &str,
    config: &RegisteredHooks,
    base_input: Value,
    base_env: Vec<(String, String)>,
) -> EnvHookExecutionResult {
    if old_cwd == new_cwd {
        return EnvHookExecutionResult::default();
    }
    if !has_env_hooks(Some(config)) {
        return EnvHookExecutionResult::default();
    }

    {
        let mut state = FILE_CHANGED_WATCHER_STATE
            .lock()
            .expect("file changed watcher poisoned");
        state.current_cwd = new_cwd.to_string();
    }

    // CC calls `clearCwdEnvFiles()` here. `sessionEnvironment.ts` is not ported
    // yet, so this remains a documented no-op until env-file support exists.
    let result = execute_cwd_changed_hooks_with_config(
        config,
        old_cwd,
        new_cwd,
        base_input,
        base_env,
        TOOL_HOOK_EXECUTION_TIMEOUT_MS,
    )
    .await;

    {
        let mut state = FILE_CHANGED_WATCHER_STATE
            .lock()
            .expect("file changed watcher poisoned");
        state.dynamic_watch_paths = result.watch_paths.clone();
        state.dynamic_watch_paths_sorted = result.watch_paths.clone();
        state.dynamic_watch_paths_sorted.sort();
        if state.initialized {
            restart_watching_locked(&mut state, Some(config));
        }
    }

    notify_results(&result);
    result
}

/// Maps to: CC `onCwdChangedForHooks(oldCwd, newCwd)` using current snapshot.
pub async fn on_cwd_changed_for_hooks(old_cwd: &str, new_cwd: &str) -> EnvHookExecutionResult {
    let Some(config) = registered_snapshot() else {
        return EnvHookExecutionResult::default();
    };
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    let context = crate::services::hooks::HookContext {
        cwd,
        ..Default::default()
    };
    on_cwd_changed_for_hooks_with_config(
        old_cwd,
        new_cwd,
        &config,
        crate::services::hooks::create_base_hook_input(&context),
        Vec::new(),
    )
    .await
}

/// Maps to: CC `dispose()`.
pub fn dispose_file_changed_watcher() {
    let mut state = FILE_CHANGED_WATCHER_STATE
        .lock()
        .expect("file changed watcher poisoned");
    state.current_cwd.clear();
    state.dynamic_watch_paths.clear();
    state.dynamic_watch_paths_sorted.clear();
    state.watched_paths.clear();
    state.initialized = false;
    state.has_env_hooks = false;
    state.notify_callback = None;
}

/// Maps to: CC `resetFileChangedWatcherForTesting()`.
pub fn reset_file_changed_watcher_for_testing() {
    dispose_file_changed_watcher();
}

pub fn file_changed_watcher_snapshot() -> FileChangedWatcherSnapshot {
    let state = FILE_CHANGED_WATCHER_STATE
        .lock()
        .expect("file changed watcher poisoned");
    FileChangedWatcherSnapshot {
        current_cwd: state.current_cwd.clone(),
        dynamic_watch_paths: state.dynamic_watch_paths.clone(),
        watched_paths: state.watched_paths.clone(),
        initialized: state.initialized,
        has_env_hooks: state.has_env_hooks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TestStateLock;
    use serde_json::Map;

    static TEST_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

    fn config(value: Value) -> RegisteredHooks {
        let config: crate::services::hooks::HooksConfig = serde_json::from_value(value).unwrap();
        crate::services::hooks::test_support::registered_config(&config)
    }

    #[test]
    fn resolve_watch_paths_combines_static_matchers_and_dynamic_paths() {
        let config = config(serde_json::json!({
            "FileChanged": [
                {"matcher": ".envrc| .env |/tmp/global.env", "hooks": [{"command": "echo ok"}]},
                {"hooks": [{"command": "echo no matcher"}]}
            ]
        }));
        let paths = resolve_watch_paths(
            Some(&config),
            "/repo",
            &["/repo/.env".to_string(), "/repo/generated.env".to_string()],
        );
        assert_eq!(
            paths,
            vec![
                "/repo/.envrc",
                "/repo/.env",
                "/tmp/global.env",
                "/repo/generated.env"
            ]
        );
    }

    #[test]
    fn initialize_and_update_watch_paths_follow_official_state_transitions() {
        let _guard = TEST_LOCK.lock().unwrap();
        // Without this the hook executor takes its untrusted-workspace early
        // return (env.rs:165-175) and every assertion below measures the skip
        // path instead of the hook.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        reset_file_changed_watcher_for_testing();
        let config = config(serde_json::json!({
            "CwdChanged": [{"hooks": [{"command": "echo cwd"}]}],
            "FileChanged": [{"matcher": ".env", "hooks": [{"command": "echo file"}]}]
        }));

        initialize_file_changed_watcher_with_config("/repo", Some(&config));
        let snapshot = file_changed_watcher_snapshot();
        assert!(snapshot.initialized);
        assert!(snapshot.has_env_hooks);
        assert_eq!(snapshot.watched_paths, vec!["/repo/.env"]);

        update_watch_paths_with_config(vec!["/repo/dynamic.env".to_string()], Some(&config));
        let snapshot = file_changed_watcher_snapshot();
        assert_eq!(snapshot.dynamic_watch_paths, vec!["/repo/dynamic.env"]);
        assert_eq!(
            snapshot.watched_paths,
            vec!["/repo/.env", "/repo/dynamic.env"]
        );

        // Same sorted path set should not reorder dynamic paths.
        update_watch_paths_with_config(vec!["/repo/dynamic.env".to_string()], Some(&config));
        assert_eq!(
            file_changed_watcher_snapshot().dynamic_watch_paths,
            vec!["/repo/dynamic.env"]
        );
        reset_file_changed_watcher_for_testing();
    }

    #[tokio::test]
    async fn cwd_changed_updates_dynamic_paths_restarts_and_notifies() {
        let _guard = TEST_LOCK.lock().unwrap();
        // Without this the hook executor takes its untrusted-workspace early
        // return (env.rs:165-175) and every assertion below measures the skip
        // path instead of the hook.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        reset_file_changed_watcher_for_testing();
        let config = config(serde_json::json!({
            "CwdChanged": [{"hooks": [{"command": "printf '{\"systemMessage\":\"cwd ok\",\"hookSpecificOutput\":{\"hookEventName\":\"CwdChanged\",\"watchPaths\":[\"/new/dyn.env\"]}}'", "timeout": 5}]}],
            "FileChanged": [{"matcher": ".env", "hooks": [{"command": "echo file"}]}]
        }));
        initialize_file_changed_watcher_with_config("/old", Some(&config));
        let notifications = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
        let notifications_for_cb = Arc::clone(&notifications);
        set_env_hook_notifier(Some(Arc::new(move |text, is_error| {
            notifications_for_cb
                .lock()
                .expect("notifications")
                .push((text, is_error));
        })));

        let result = on_cwd_changed_for_hooks_with_config(
            "/old",
            "/new",
            &config,
            Value::Object(Map::new()),
            vec![],
        )
        .await;

        assert_eq!(result.watch_paths, vec!["/new/dyn.env"]);
        let snapshot = file_changed_watcher_snapshot();
        assert_eq!(snapshot.current_cwd, "/new");
        assert_eq!(snapshot.dynamic_watch_paths, vec!["/new/dyn.env"]);
        assert_eq!(snapshot.watched_paths, vec!["/new/.env", "/new/dyn.env"]);
        assert_eq!(
            notifications.lock().expect("notifications").as_slice(),
            &[("cwd ok".to_string(), false)]
        );
        reset_file_changed_watcher_for_testing();
    }

    #[tokio::test]
    async fn file_event_updates_dynamic_paths_and_reports_failed_output() {
        let _guard = TEST_LOCK.lock().unwrap();
        // Without this the hook executor takes its untrusted-workspace early
        // return (env.rs:165-175) and every assertion below measures the skip
        // path instead of the hook.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        reset_file_changed_watcher_for_testing();
        let config = config(serde_json::json!({
            "FileChanged": [{"matcher": ".env", "hooks": [
                {"command": "printf '{\"hookSpecificOutput\":{\"hookEventName\":\"FileChanged\",\"watchPaths\":[\"/repo/next.env\"]}}'", "timeout": 5},
                {"command": "sh -c 'echo bad >&2; exit 1'", "timeout": 5}
            ]}]
        }));
        initialize_file_changed_watcher_with_config("/repo", Some(&config));
        let notifications = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
        let notifications_for_cb = Arc::clone(&notifications);
        set_env_hook_notifier(Some(Arc::new(move |text, is_error| {
            notifications_for_cb
                .lock()
                .expect("notifications")
                .push((text, is_error));
        })));

        let result = handle_file_event_with_config(
            "/repo/.env",
            "change",
            &config,
            Value::Object(Map::new()),
            vec![],
        )
        .await;

        assert_eq!(result.results.len(), 2);
        assert_eq!(
            file_changed_watcher_snapshot().dynamic_watch_paths,
            vec!["/repo/next.env"]
        );
        assert_eq!(
            notifications.lock().expect("notifications").as_slice(),
            &[("bad\n".to_string(), true)]
        );
        reset_file_changed_watcher_for_testing();
    }

    #[tokio::test]
    async fn cwd_changed_noops_for_same_path_or_no_env_hooks() {
        let _guard = TEST_LOCK.lock().unwrap();
        // Without this the hook executor takes its untrusted-workspace early
        // return (env.rs:165-175) and every assertion below measures the skip
        // path instead of the hook.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        reset_file_changed_watcher_for_testing();
        let config = config(serde_json::json!({"Stop": [{"hooks": [{"command": "echo stop"}]}]}));
        let same = on_cwd_changed_for_hooks_with_config(
            "/repo",
            "/repo",
            &config,
            Value::Object(Map::new()),
            vec![],
        )
        .await;
        assert!(same.results.is_empty());
        let no_hooks = on_cwd_changed_for_hooks_with_config(
            "/a",
            "/b",
            &config,
            Value::Object(Map::new()),
            vec![],
        )
        .await;
        assert!(no_hooks.results.is_empty());
        reset_file_changed_watcher_for_testing();
    }
}
