//! Global LSP server manager singleton.
//!
//! Maps to: CC `services/lsp/manager.ts`.
//!
//! This module owns initialization state and the singleton manager handle. It
//! deliberately keeps protocol/runtime work in `server_manager` /
//! `server_instance`, matching the official service boundary; tools call this
//! module only to wait for initialization and route requests.
//!
//! Two shapes here are load-bearing and were previously inverted:
//!
//! 1. **Sharing.** CC holds ONE `lspManagerInstance` (`manager.ts:20`) and
//!    `getLspServerManager()` hands the same reference to every caller. This
//!    module clones an `Arc` out of the slot; it never `take()`s the manager for
//!    the duration of a call. The old handoff made any concurrent reader — the
//!    LSP tool (`isConcurrencySafe → true`, run in parallel by
//!    `streaming_tool_executor`), the Write/Edit didChange/didSave spawns, the
//!    notification poll — observe an empty slot and answer with "No LSP server
//!    available for file type: …" or "LSP server manager not initialized".
//!
//! 2. **Asynchrony.** CC creates the instance synchronously, publishes
//!    `'pending'`, and runs `initialize()` as a BACKGROUND promise stored in
//!    `initializationPromise` (`manager.ts:167-207`, docblock: "starts async
//!    initialization … without blocking the startup process"). That is not a
//!    latency optimisation: `'pending'` is state three consumers branch on
//!    (`LSPTool.call` → `waitForInitialization()`, the notification poll, and
//!    `shouldDeferLspTool`). Running `initialize()` synchronously under the
//!    mutex made `'pending'` unobservable, `wait_for_initialization()` an empty
//!    function, and the whole `failed` path unreachable.

use crate::services::lsp::server_manager::{
    LspServerManager, LspServerSnapshot, create_lsp_server_manager,
};
use indexmap::IndexMap;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::watch;

/// Maps to CC `InitializationState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InitializationState {
    #[default]
    NotStarted,
    Pending,
    Success,
    Failed,
}

/// Maps to: CC `getInitializationStatus()` return union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitializationStatus {
    NotStarted,
    Pending,
    Success,
    Failed { error: String },
}

#[derive(Default)]
struct GlobalLspManagerState {
    manager: Option<Arc<LspServerManager>>,
    initialization_state: InitializationState,
    initialization_error: Option<String>,
    initialization_generation: u64,
    /// Maps to: CC `initializationPromise` (`manager.ts:40`) — resolves when the
    /// CURRENT generation's background initialization settles, success or
    /// failure. A `watch` receiver rather than a stored future so several
    /// awaiting callers share one completion signal.
    initialization_complete: Option<watch::Receiver<bool>>,
}

static GLOBAL_MANAGER: LazyLock<Mutex<GlobalLspManagerState>> =
    LazyLock::new(|| Mutex::new(GlobalLspManagerState::default()));

/// Maps to: CC `getLspServerManager()` — the SHARED reference, never a handoff.
/// Note CC returns the instance while initialization is still `'pending'`; only
/// `'failed'` withholds it (`manager.ts:63-69`).
fn get_lsp_server_manager() -> Option<Arc<LspServerManager>> {
    let state = GLOBAL_MANAGER.lock().unwrap();
    if state.initialization_state == InitializationState::Failed {
        return None;
    }
    state.manager.clone()
}

/// Maps to: CC `getLspServerManager() !== undefined` at its call sites.
pub fn has_lsp_server_manager() -> bool {
    get_lsp_server_manager().is_some()
}

/// Maps to: CC `getInitializationStatus()`.
pub fn get_initialization_status() -> InitializationStatus {
    let state = GLOBAL_MANAGER.lock().unwrap();
    match state.initialization_state {
        InitializationState::NotStarted => InitializationStatus::NotStarted,
        InitializationState::Pending => InitializationStatus::Pending,
        InitializationState::Success => InitializationStatus::Success,
        InitializationState::Failed => InitializationStatus::Failed {
            error: state
                .initialization_error
                .clone()
                .unwrap_or_else(|| "Initialization failed".to_string()),
        },
    }
}

/// Maps to: CC `isLspConnected()`.
pub fn is_lsp_connected() -> bool {
    let Some(manager) = get_lsp_server_manager() else {
        return false;
    };
    let servers = manager.get_all_servers();
    !servers.is_empty()
        && servers
            .iter()
            .any(|(_, server)| server.state() != crate::services::lsp::types::LspServerState::Error)
}

/// Maps to: CC `waitForInitialization()` (`manager.ts:121-133`).
pub async fn wait_for_initialization() {
    let receiver = {
        let state = GLOBAL_MANAGER.lock().unwrap();
        match state.initialization_state {
            // Already settled, or never started: nothing to wait for.
            InitializationState::Success
            | InitializationState::Failed
            | InitializationState::NotStarted => return,
            InitializationState::Pending => state.initialization_complete.clone(),
        }
    };
    if let Some(mut receiver) = receiver {
        // `Err` = the sender was dropped without publishing, i.e. the detached
        // initialization task was destroyed. CC's promise settles either way.
        let _ = receiver.wait_for(|complete| *complete).await;
    }
}

/// Maps to: CC `initializeLspServerManager()`.
pub fn initialize_lsp_server_manager() {
    // Maps to: CC `isBareMode()` guard (`manager.ts:148`), which covers both
    // `--bare` and the SIMPLE env var.
    if crate::utils::env_utils::is_bare_mode() {
        return;
    }
    initialize_lsp_server_manager_with(create_lsp_server_manager());
}

/// The body of `initializeLspServerManager()` after `createLSPServerManager()`.
/// Split out so a test can substitute the manager (CC tests mock the factory
/// module) without bypassing any of the state machine below.
fn initialize_lsp_server_manager_with(manager: LspServerManager) {
    let manager = Arc::new(manager);
    let (complete_sender, complete_receiver) = watch::channel(false);

    let generation = {
        let mut state = GLOBAL_MANAGER.lock().unwrap();
        // Maps to: CC `:154-159` — already initialized or initializing.
        if state.manager.is_some() && state.initialization_state != InitializationState::Failed {
            return;
        }
        // Maps to: CC `:162-165` — reset for retry after a failure.
        if state.initialization_state == InitializationState::Failed {
            state.manager = None;
            state.initialization_error = None;
        }

        // Maps to: CC `:168-170` — the instance is published BEFORE
        // initialization runs, so `getLspServerManager()` is non-undefined
        // while the state is `'pending'`.
        state.manager = Some(Arc::clone(&manager));
        state.initialization_state = InitializationState::Pending;
        state.initialization_generation = state.initialization_generation.saturating_add(1);
        state.initialization_complete = Some(complete_receiver);
        state.initialization_generation
    };

    // Maps to: CC `:180-207` — `initializationPromise = manager.initialize()
    // .then(...).catch(...)`, started and NOT awaited by the caller
    // (`main.tsx:3356`).
    let task = async move {
        let result = manager.initialize().await;
        let settled_manager = {
            let mut state = GLOBAL_MANAGER.lock().unwrap();
            // Maps to: CC's `currentGeneration === initializationGeneration`
            // guard — a stale initialization must not overwrite live state.
            if state.initialization_generation != generation {
                None
            } else {
                match result {
                    Ok(()) => {
                        state.initialization_state = InitializationState::Success;
                        state.initialization_error = None;
                        state.manager.clone()
                    }
                    Err(error) => {
                        state.initialization_state = InitializationState::Failed;
                        state.initialization_error = Some(error.to_string());
                        // Maps to: CC `:200` — clear the unusable instance.
                        state.manager = None;
                        None
                    }
                }
            }
        };
        // Maps to: CC `:188-191` — passive diagnostics are registered after the
        // state flips to success, outside the state update itself.
        if let Some(manager) = settled_manager {
            let _ = crate::services::lsp::passive_feedback::register_lsp_notification_handlers(
                &manager,
            );
        }
        let _ = complete_sender.send(true);
    };

    match crate::utils::process_runtime::runtime_handle_for_detached_work() {
        Some(handle) => {
            handle.spawn(task);
        }
        None => {
            // No process runtime published (unit tests, embedded callers). CC
            // always has its event loop; running the initialization inline is
            // the only way to keep this function total, at the cost of
            // `'pending'` never being externally observable in that shape.
            let _ = crate::utils::process_runtime::block_on_from_sync(task);
        }
    }
}

/// Same seam as [`initialize_lsp_server_manager_with`], for tests in sibling
/// modules that need to observe the `pending` window from a consumer's side.
#[cfg(test)]
pub(crate) fn initialize_lsp_server_manager_for_testing(manager: LspServerManager) {
    initialize_lsp_server_manager_with(manager);
}

/// Maps to: CC `reinitializeLspServerManager()`.
pub fn reinitialize_lsp_server_manager() {
    let previous = {
        let state = GLOBAL_MANAGER.lock().unwrap();
        if state.initialization_state == InitializationState::NotStarted {
            // Maps to: CC `:227-231` — never initialized, don't start now.
            return;
        }
        state.manager.clone()
    };

    // Maps to: CC `:238-244` `void lspManagerInstance.shutdown().catch(...)` —
    // best-effort, fire-and-forget, so `/reload-plugins` does not leak child
    // processes while the new instance is already being built.
    if let Some(previous) = previous {
        if let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
            handle.spawn(async move {
                let _ = previous.shutdown().await;
            });
        }
    }

    {
        let mut state = GLOBAL_MANAGER.lock().unwrap();
        state.manager = None;
        state.initialization_state = InitializationState::NotStarted;
        state.initialization_error = None;
        state.initialization_generation = state.initialization_generation.saturating_add(1);
        state.initialization_complete = None;
    }
    initialize_lsp_server_manager();
}

/// Maps to: CC `shutdownLspServerManager()`.
pub async fn shutdown_lsp_server_manager() -> anyhow::Result<()> {
    // Maps to: CC's `finally` block clearing the singleton (`:280-288`). This is
    // a clear, not an ownership handoff — the manager is gone afterwards.
    let manager = {
        let mut state = GLOBAL_MANAGER.lock().unwrap();
        state.initialization_state = InitializationState::NotStarted;
        state.initialization_error = None;
        state.initialization_generation = state.initialization_generation.saturating_add(1);
        state.initialization_complete = None;
        state.manager.take()
    };

    if let Some(manager) = manager {
        manager.shutdown().await?;
    }
    Ok(())
}

/// Maps to: CC `_resetLspManagerForTesting()`.
pub fn reset_lsp_manager_for_testing() {
    let mut state = GLOBAL_MANAGER.lock().unwrap();
    state.manager = None;
    state.initialization_state = InitializationState::NotStarted;
    state.initialization_error = None;
    state.initialization_generation = state.initialization_generation.saturating_add(1);
    state.initialization_complete = None;
}

/// Maps to: CC `getLspServerManager().getAllServers()` as consumed by the
/// initialization notification hook.
pub fn get_all_servers_snapshot() -> Option<IndexMap<String, LspServerSnapshot>> {
    GLOBAL_MANAGER
        .lock()
        .unwrap()
        .manager
        .as_ref()
        .map(|manager| manager.get_all_server_snapshots())
}

/// Maps to: CC `getLspServerManager().isFileOpen(filePath)`.
pub fn is_file_open(file_path: &str) -> bool {
    get_lsp_server_manager().is_some_and(|manager| manager.is_file_open(file_path))
}

/// Maps to: CC `getLspServerManager().openFile(filePath, content)`.
pub async fn open_file(file_path: &str, content: String) -> anyhow::Result<()> {
    match get_lsp_server_manager() {
        Some(manager) => manager.open_file(file_path, content).await,
        None => Ok(()),
    }
}

/// Maps to: CC `getLspServerManager().changeFile(filePath, content)`.
pub async fn change_file(file_path: &str, content: String) -> anyhow::Result<()> {
    match get_lsp_server_manager() {
        Some(manager) => manager.change_file(file_path, content).await,
        None => Ok(()),
    }
}

/// Maps to: CC `getLspServerManager().saveFile(filePath)`.
pub async fn save_file(file_path: &str) -> anyhow::Result<()> {
    match get_lsp_server_manager() {
        Some(manager) => manager.save_file(file_path).await,
        None => Ok(()),
    }
}

/// Maps to: CC `getLspServerManager().closeFile(filePath)`.
pub async fn close_file(file_path: &str) -> anyhow::Result<()> {
    match get_lsp_server_manager() {
        Some(manager) => manager.close_file(file_path).await,
        None => Ok(()),
    }
}

/// Maps to: CC `getLspServerManager().sendRequest(filePath, method, params)`.
pub async fn send_request(
    file_path: &str,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<Option<serde_json::Value>> {
    match get_lsp_server_manager() {
        Some(manager) => manager.send_request(file_path, method, params).await,
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::lsp::config::AllLspServers;
    use crate::services::lsp::server_manager::create_lsp_server_manager_with_config_loader;
    use crate::utils::test_env::TEST_ENV_LOCK;

    /// Guard so a test can hold the manager singleton across `.await` points
    /// without the env lock's poison semantics leaking into other assertions.
    fn reset_manager() {
        reset_lsp_manager_for_testing();
    }

    #[test]
    fn initialization_status_starts_not_started() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_lsp_manager_for_testing();
        assert_eq!(
            get_initialization_status(),
            InitializationStatus::NotStarted
        );
    }

    #[test]
    fn initialize_creates_empty_manager_from_safe_plugin_loader_boundary() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");
        reset_lsp_manager_for_testing();
        initialize_lsp_server_manager();

        assert_eq!(get_initialization_status(), InitializationStatus::Success);
        assert!(has_lsp_server_manager());
        assert!(!is_lsp_connected());
        assert!(get_all_servers_snapshot().unwrap().is_empty());
        reset_lsp_manager_for_testing();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn file_sync_wrappers_are_noops_without_matching_server_like_official_manager() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");
        reset_lsp_manager_for_testing();
        initialize_lsp_server_manager();

        change_file("/tmp/example.rs", "fn main() {}\n".to_string())
            .await
            .unwrap();
        save_file("/tmp/example.rs").await.unwrap();
        close_file("/tmp/example.rs").await.unwrap();
        assert_eq!(get_initialization_status(), InitializationStatus::Success);
        assert!(has_lsp_server_manager());
        reset_lsp_manager_for_testing();
    }

    #[test]
    fn bare_mode_skips_initialization_like_official() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_lsp_manager_for_testing();
        crate::utils::process_env::set("CLAUDE_CODE_SIMPLE", "1");
        initialize_lsp_server_manager();
        assert_eq!(
            get_initialization_status(),
            InitializationStatus::NotStarted
        );
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");
        reset_lsp_manager_for_testing();
    }

    /// H2: `'pending'` must be EXTERNALLY OBSERVABLE — CC creates the instance,
    /// publishes `'pending'`, then runs `initialize()` in the background
    /// (`manager.ts:167-181`). The loader below blocks until released, so the
    /// assertions run while initialization is genuinely in flight.
    ///
    /// This also revives consumer #1, `waitForInitialization()`
    /// (`LSPTool.ts:230-233`): it must not return until the load settles.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_is_observable_and_wait_for_initialization_blocks_on_it() {
        reset_manager();
        let (release_sender, release_receiver) = std::sync::mpsc::channel::<()>();
        let release_receiver = std::sync::Mutex::new(release_receiver);
        let manager = create_lsp_server_manager_with_config_loader(Arc::new(move || {
            let _ = release_receiver.lock().unwrap().recv();
            Ok(AllLspServers::default())
        }));

        initialize_lsp_server_manager_with(manager);

        assert_eq!(get_initialization_status(), InitializationStatus::Pending);
        // CC returns the instance while pending — only 'failed' withholds it.
        assert!(has_lsp_server_manager());

        let waiter = tokio::spawn(async {
            wait_for_initialization().await;
            get_initialization_status()
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !waiter.is_finished(),
            "waitForInitialization() must block while pending"
        );
        assert_eq!(get_initialization_status(), InitializationStatus::Pending);

        // Consumer #2, the notification poll: CC's `'pending'` arm returns
        // without notifying and keeps polling
        // (`useLspInitializationNotification.tsx:116-119`). Unreachable while
        // initialization ran synchronously under the mutex.
        let mut notified = std::collections::HashSet::new();
        let pending_poll =
            crate::hooks::notifs::use_lsp_initialization_notification::poll_lsp_initialization_notifications(
                &mut notified,
            );
        assert!(pending_poll.notifications.is_empty());
        assert!(pending_poll.should_continue_polling);

        let _ = release_sender.send(());
        let status_after_wait = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("waiter must resolve once initialization settles")
            .unwrap();

        assert_eq!(status_after_wait, InitializationStatus::Success);
        assert_eq!(get_initialization_status(), InitializationStatus::Success);
        reset_manager();
    }

    /// H3: the `failed` transition must be reachable through the production
    /// state machine. CC's `initialize()` rethrows a config-load failure
    /// (`LSPServerManager.ts:80-86`) and the `.catch` sets `'failed'`, clears
    /// the instance, and stops the poll (`manager.ts:194-207`).
    ///
    /// This revives consumers #2 and #3: `getLspServerManager()` withholds the
    /// instance, and the notification poll emits "LSP for lsp-manager failed"
    /// and stops — from a real transition, not a hand-built status value.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_initialization_clears_the_instance_and_drives_the_official_poll() {
        reset_manager();
        let manager = create_lsp_server_manager_with_config_loader(Arc::new(|| {
            anyhow::bail!("plugin cache unreadable")
        }));

        initialize_lsp_server_manager_with(manager);
        wait_for_initialization().await;

        assert_eq!(
            get_initialization_status(),
            InitializationStatus::Failed {
                error: "plugin cache unreadable".to_string()
            }
        );
        assert!(
            !has_lsp_server_manager(),
            "a failed manager must not be handed out (manager.ts:64-67)"
        );
        assert!(!is_lsp_connected());

        let mut notified = std::collections::HashSet::new();
        let outcome =
            crate::hooks::notifs::use_lsp_initialization_notification::poll_lsp_initialization_notifications(
                &mut notified,
            );
        assert!(!outcome.should_continue_polling);
        assert_eq!(outcome.notifications.len(), 1);
        assert_eq!(
            outcome.notifications[0].text,
            "LSP for lsp-manager failed · /plugin for details"
        );

        // Maps to CC `:143` — "if initialization previously failed, calling
        // again will retry".
        initialize_lsp_server_manager_with(create_lsp_server_manager_with_config_loader(Arc::new(
            || Ok(AllLspServers::default()),
        )));
        wait_for_initialization().await;
        assert_eq!(get_initialization_status(), InitializationStatus::Success);
        reset_manager();
    }

    /// The generation guard (`manager.ts:33-35`, `:184`): a slow initialization
    /// that settles after the singleton was reset must not resurrect it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_initialization_does_not_overwrite_a_reset_singleton() {
        reset_manager();
        let (release_sender, release_receiver) = std::sync::mpsc::channel::<()>();
        let release_receiver = std::sync::Mutex::new(release_receiver);
        initialize_lsp_server_manager_with(create_lsp_server_manager_with_config_loader(Arc::new(
            move || {
                let _ = release_receiver.lock().unwrap().recv();
                Ok(AllLspServers::default())
            },
        )));
        assert_eq!(get_initialization_status(), InitializationStatus::Pending);

        reset_manager();
        let _ = release_sender.send(());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_eq!(
            get_initialization_status(),
            InitializationStatus::NotStarted,
            "a stale generation must not publish success over a reset"
        );
        assert!(!has_lsp_server_manager());
        reset_manager();
    }

    /// H1: the singleton is SHARED, never handed off. The configured server
    /// below fails to spawn, so every `send_request` genuinely awaits a process
    /// spawn — a wide window during which the old `take()`/put-back shape left
    /// the slot empty and concurrent callers got the false negatives
    /// "No LSP server available for file type: .rs" (`send_request` resolving
    /// to `Ok(None)`) and "LSP server manager not initialized"
    /// (`has_lsp_server_manager()` returning false).
    ///
    /// Reachable in production because `LspTool::is_concurrency_safe` is true,
    /// so `streaming_tool_executor` runs LSP calls in parallel, and Write/Edit
    /// `tokio::spawn` their didChange/didSave pair.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_manager_routing_never_observes_an_empty_slot() {
        use crate::services::lsp::types::{LspServerConfig, ScopedLspServerConfig};

        reset_manager();
        initialize_lsp_server_manager_with(create_lsp_server_manager_with_config_loader(Arc::new(
            || {
                let mut extension_to_language = std::collections::BTreeMap::new();
                extension_to_language.insert(".rs".to_string(), "rust".to_string());
                let mut servers = IndexMap::new();
                servers.insert(
                    "rust".to_string(),
                    ScopedLspServerConfig {
                        config: LspServerConfig {
                            command: "definitely-not-a-cometix-lsp-server".to_string(),
                            extension_to_language,
                            ..LspServerConfig::default()
                        },
                        server_name: Some("rust".to_string()),
                        scope: None,
                        source: None,
                    },
                );
                Ok(AllLspServers { servers })
            },
        )));
        wait_for_initialization().await;
        assert_eq!(get_initialization_status(), InitializationStatus::Success);

        let mut tasks = Vec::new();
        for index in 0..8 {
            tasks.push(tokio::spawn(async move {
                let path = format!("/tmp/cometix-lsp-concurrency-{index}.rs");
                for _ in 0..10 {
                    // The routed call fails (that binary does not exist), but it
                    // must never resolve to `Ok(None)` — that is the "no server
                    // for this file type" answer, and a routed `.rs` file always
                    // has one.
                    let routed =
                        send_request(&path, "textDocument/hover", serde_json::json!({})).await;
                    assert!(
                        routed.is_err(),
                        "a routed .rs request must reach the server, not fall through to Ok(None)"
                    );
                    let _ = change_file(&path, "fn main() {}".to_string()).await;
                    let _ = save_file(&path).await;
                    assert!(
                        has_lsp_server_manager(),
                        "the manager slot must never be empty while calls are in flight"
                    );
                    assert_eq!(
                        get_all_servers_snapshot()
                            .expect("server snapshot must stay readable")
                            .len(),
                        1,
                        "the notification poll must keep seeing the configured server"
                    );
                    assert!(!is_file_open(&path));
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        assert_eq!(get_initialization_status(), InitializationStatus::Success);
        assert!(has_lsp_server_manager());
        reset_manager();
    }
}
