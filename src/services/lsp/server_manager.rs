//! LSP server manager.
//!
//! Maps to: CC `services/lsp/LSPServerManager.ts`.
//!
//! The manager owns configured server instances, extension routing, and file
//! open bookkeeping. Raw stdio JSON-RPC transport stays inside
//! `client.rs`/`server_instance.rs`; tools reach servers only through this
//! manager boundary.
//!
//! Ownership shape: CC's `createLSPServerManager()` closes over three `Map`s
//! and `manager.ts:20` keeps ONE instance that every caller shares
//! (`getLspServerManager()` hands out the same reference). This port mirrors
//! that — one `Arc<LspServerManager>` with interior mutability — rather than
//! moving the manager out of its global slot for the duration of a request.
//! With the old `&mut self` shape a concurrent caller found an empty slot and
//! got "No LSP server available for file type: .rs" / "LSP server manager not
//! initialized" while a request was merely in flight.

use crate::services::lsp::config::{AllLspServers, get_all_lsp_servers};
use crate::services::lsp::server_instance::LspServerInstance;
use crate::services::lsp::types::{LspServerState, ScopedLspServerConfig};
use crate::utils::debug::log_for_debugging;
use indexmap::IndexMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Public server snapshot used by notification hooks and tests.
/// Maps to fields read from CC `LSPServerInstance` by
/// `hooks/notifs/useLspInitializationNotification.tsx`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspServerSnapshot {
    pub name: String,
    pub state: LspServerState,
    pub last_error: Option<String>,
}

/// Maps to: CC `initialize()`'s `await getAllLspServers()` — the one step the
/// official manager declares as `@throws {Error} If configuration loading
/// fails` and rethrows (`LSPServerManager.ts:80-86`), which is what puts the
/// singleton into `'failed'`.
type LspConfigLoader = Arc<dyn Fn() -> anyhow::Result<AllLspServers> + Send + Sync>;

/// CC's three closure `Map`s. `IndexMap` (not `BTreeMap`) because CC relies on
/// insertion order: `getServerForFile` takes `serverNames[0]`
/// (`LSPServerManager.ts:201`), so when two servers claim the same extension
/// the FIRST registered wins, not the lexicographically smallest.
#[derive(Debug, Default)]
struct LspServerManagerState {
    servers: IndexMap<String, Arc<LspServerInstance>>,
    extension_map: IndexMap<String, Vec<String>>,
    opened_files: IndexMap<String, String>,
}

/// Maps to the `LSPServerManager` interface returned by CC
/// `createLSPServerManager()`.
pub struct LspServerManager {
    state: Mutex<LspServerManagerState>,
    load_configs: LspConfigLoader,
}

impl std::fmt::Debug for LspServerManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspServerManager")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// Maps to: CC `createLSPServerManager()`.
pub fn create_lsp_server_manager() -> LspServerManager {
    LspServerManager {
        state: Mutex::new(LspServerManagerState::default()),
        load_configs: Arc::new(|| Ok(get_all_lsp_servers())),
    }
}

/// Same factory with the config-loading step substituted. CC tests mock the
/// `./config.js` module; this is the same seam one level in, so the whole
/// `pending → success/failed` state machine in `manager.rs` stays the
/// production code path — only the leaf loader is replaced.
#[cfg(test)]
pub(crate) fn create_lsp_server_manager_with_config_loader(
    load_configs: LspConfigLoader,
) -> LspServerManager {
    LspServerManager {
        state: Mutex::new(LspServerManagerState::default()),
        load_configs,
    }
}

impl LspServerManager {
    /// Maps to: CC `LSPServerManager.initialize()`.
    pub async fn initialize(&self) -> anyhow::Result<()> {
        let configs = match (self.load_configs)() {
            Ok(configs) => configs,
            Err(error) => {
                // Maps to: CC `:80-86` — logged, then RETHROWN, which is what
                // moves the singleton to `'failed'`.
                log_for_debugging(&format!("Failed to load LSP server configuration: {error}"));
                return Err(error);
            }
        };
        self.initialize_from_configs(configs.servers);
        Ok(())
    }

    /// Test/support seam for the official manager's post-config state build.
    /// Maps to the body of CC `initialize()` after `getAllLspServers()` returns.
    pub fn initialize_from_configs(&self, server_configs: IndexMap<String, ScopedLspServerConfig>) {
        let mut state = self.state.lock().unwrap();
        state.servers.clear();
        state.extension_map.clear();
        state.opened_files.clear();

        // CC wraps each server's body in try/catch and `continue`s on throw
        // (`:90-145`); every `continue` below is one of those throws, with the
        // same `logError` it produces.
        for (server_name, config) in server_configs {
            if config.config.command.is_empty() {
                log_for_debugging(&format!(
                    "Failed to initialize LSP server {server_name}: Server {server_name} missing required 'command' field"
                ));
                continue;
            }
            if config.config.extension_to_language.is_empty() {
                log_for_debugging(&format!(
                    "Failed to initialize LSP server {server_name}: Server {server_name} missing required 'extensionToLanguage' field"
                ));
                continue;
            }

            // Order matters and is CC's: extensions are registered BEFORE
            // `createLSPServerInstance` (`:107-120`), so a server that fails to
            // construct leaves its extension mapped to a name that is absent
            // from `servers` — shadowing any later server for that extension,
            // which then answers "No LSP server available for file type". The
            // port used to `continue` before touching the map, silently
            // improving on CC; that divergence is removed here.
            for ext in config.config.extension_to_language.keys() {
                state
                    .extension_map
                    .entry(ext.to_lowercase())
                    .or_default()
                    .push(server_name.clone());
            }

            if let Some(error) = LspServerInstance::unsupported_config_error(&server_name, &config)
            {
                log_for_debugging(&format!(
                    "Failed to initialize LSP server {server_name}: {error}"
                ));
                continue;
            }

            let instance = LspServerInstance::new(server_name.clone(), config);
            // Maps to: CC `LSPServerManager.initialize()` registration of a
            // `workspace/configuration` handler for servers that request it.
            instance.on_request("workspace/configuration", |params| {
                let count = params
                    .get("items")
                    .and_then(|items| items.as_array())
                    .map(|items| items.len())
                    .unwrap_or(0);
                serde_json::Value::Array(vec![serde_json::Value::Null; count])
            });
            state.servers.insert(server_name, Arc::new(instance));
        }
        log_for_debugging(&format!(
            "LSP manager initialized with {} servers",
            state.servers.len()
        ));
    }

    /// Maps to: CC `LSPServerManager.shutdown()`.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let servers: Vec<(String, Arc<LspServerInstance>)> = {
            let mut state = self.state.lock().unwrap();
            let servers = std::mem::take(&mut state.servers).into_iter().collect();
            state.extension_map.clear();
            state.opened_files.clear();
            servers
        };
        let mut errors = Vec::new();
        for (name, server) in servers {
            if matches!(
                server.state(),
                LspServerState::Running | LspServerState::Error
            ) {
                if let Err(error) = server.stop().await {
                    errors.push(format!("{name}: {error}"));
                }
            }
        }

        if !errors.is_empty() {
            anyhow::bail!(
                "Failed to stop {} LSP server(s): {}",
                errors.len(),
                errors.join("; ")
            );
        }
        Ok(())
    }

    /// Maps to: CC `LSPServerManager.getServerForFile(filePath)`.
    pub fn get_server_for_file(&self, file_path: &str) -> Option<Arc<LspServerInstance>> {
        let ext = file_extension(file_path)?;
        let state = self.state.lock().unwrap();
        let server_name = state.extension_map.get(&ext)?.first()?;
        state.servers.get(server_name).cloned()
    }

    /// Maps to: CC `LSPServerManager.ensureServerStarted(filePath)`.
    pub async fn ensure_server_started(
        &self,
        file_path: &str,
    ) -> anyhow::Result<Option<Arc<LspServerInstance>>> {
        let Some(server) = self.get_server_for_file(file_path) else {
            return Ok(None);
        };
        if matches!(
            server.state(),
            LspServerState::Stopped | LspServerState::Error
        ) {
            server.start().await?;
        }
        Ok(Some(server))
    }

    /// Maps to: CC `LSPServerManager.sendRequest(filePath, method, params)`.
    pub async fn send_request(
        &self,
        file_path: &str,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let Some(server) = self.ensure_server_started(file_path).await? else {
            return Ok(None);
        };
        server.send_request(method, params).await.map(Some)
    }

    /// Maps to: CC `LSPServerManager.openFile(filePath, content)`.
    pub async fn open_file(&self, file_path: &str, content: String) -> anyhow::Result<()> {
        let file_uri = path_to_file_url(Path::new(file_path));
        let Some(server) = self.ensure_server_started(file_path).await? else {
            return Ok(());
        };
        if self.opened_file_server(&file_uri).as_deref() == Some(server.name.as_str()) {
            return Ok(());
        }

        let ext = file_extension(file_path).unwrap_or_default();
        let language_id = server
            .config
            .config
            .extension_to_language
            .get(&ext)
            .cloned()
            .unwrap_or_else(|| "plaintext".to_string());
        server
            .send_notification(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {
                        "uri": file_uri,
                        "languageId": language_id,
                        "version": 1,
                        "text": content,
                    }
                }),
            )
            .await?;
        self.state
            .lock()
            .unwrap()
            .opened_files
            .insert(file_uri, server.name.clone());
        Ok(())
    }

    /// Maps to: CC `LSPServerManager.changeFile(filePath, content)`.
    pub async fn change_file(&self, file_path: &str, content: String) -> anyhow::Result<()> {
        let file_uri = path_to_file_url(Path::new(file_path));
        let Some(server) = self.get_server_for_file(file_path) else {
            return self.open_file(file_path, content).await;
        };
        if server.state() != LspServerState::Running {
            return self.open_file(file_path, content).await;
        }
        if self.opened_file_server(&file_uri).as_deref() != Some(server.name.as_str()) {
            return self.open_file(file_path, content).await;
        }
        server
            .send_notification(
                "textDocument/didChange",
                serde_json::json!({
                    "textDocument": { "uri": file_uri, "version": 1 },
                    "contentChanges": [{ "text": content }],
                }),
            )
            .await
    }

    /// Maps to: CC `LSPServerManager.saveFile(filePath)`.
    pub async fn save_file(&self, file_path: &str) -> anyhow::Result<()> {
        let file_uri = path_to_file_url(Path::new(file_path));
        let Some(server) = self.get_server_for_file(file_path) else {
            return Ok(());
        };
        if server.state() != LspServerState::Running {
            return Ok(());
        }
        server
            .send_notification(
                "textDocument/didSave",
                serde_json::json!({ "textDocument": { "uri": file_uri } }),
            )
            .await
    }

    /// Maps to: CC `LSPServerManager.closeFile(filePath)`.
    pub async fn close_file(&self, file_path: &str) -> anyhow::Result<()> {
        let file_uri = path_to_file_url(Path::new(file_path));
        let Some(server) = self.get_server_for_file(file_path) else {
            return Ok(());
        };
        if server.state() != LspServerState::Running {
            return Ok(());
        }
        server
            .send_notification(
                "textDocument/didClose",
                serde_json::json!({ "textDocument": { "uri": file_uri } }),
            )
            .await?;
        self.state
            .lock()
            .unwrap()
            .opened_files
            .shift_remove(&file_uri);
        Ok(())
    }

    /// Maps to: CC `LSPServerManager.isFileOpen(filePath)`.
    pub fn is_file_open(&self, file_path: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .opened_files
            .contains_key(&path_to_file_url(Path::new(file_path)))
    }

    fn opened_file_server(&self, file_uri: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .opened_files
            .get(file_uri)
            .cloned()
    }

    /// Maps to: CC `LSPServerManager.getAllServers()` — the live instances, in
    /// registration order.
    pub fn get_all_servers(&self) -> Vec<(String, Arc<LspServerInstance>)> {
        self.state
            .lock()
            .unwrap()
            .servers
            .iter()
            .map(|(name, server)| (name.clone(), Arc::clone(server)))
            .collect()
    }

    /// The read-only projection of `getAllServers()` consumed by the
    /// initialization notification hook and by tests.
    pub fn get_all_server_snapshots(&self) -> IndexMap<String, LspServerSnapshot> {
        self.get_all_servers()
            .into_iter()
            .map(|(name, server)| {
                (
                    name.clone(),
                    LspServerSnapshot {
                        name,
                        state: server.state(),
                        last_error: server.last_error(),
                    },
                )
            })
            .collect()
    }

    pub fn server_count(&self) -> usize {
        self.state.lock().unwrap().servers.len()
    }

    pub fn extensions(&self) -> BTreeSet<String> {
        self.state
            .lock()
            .unwrap()
            .extension_map
            .keys()
            .cloned()
            .collect()
    }
}

fn file_extension(file_path: &str) -> Option<String> {
    Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}").to_lowercase())
}

pub(crate) fn path_to_file_url(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = absolute.to_string_lossy().replace('\\', "/");
    if !normalized.starts_with('/') {
        normalized = format!("/{normalized}");
    }
    format!("file://{}", percent_encode_path(&normalized))
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::lsp::types::{LspServerConfig, ScopedLspServerConfig};
    use crate::utils::test_env::TEST_ENV_LOCK;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn rust_config() -> ScopedLspServerConfig {
        config_for_extension(".rs", "rust")
    }

    fn config_for_extension(extension: &str, language: &str) -> ScopedLspServerConfig {
        let mut extension_to_language = BTreeMap::new();
        extension_to_language.insert(extension.to_string(), language.to_string());
        ScopedLspServerConfig {
            config: LspServerConfig {
                command: "definitely-not-a-cometix-lsp-server".to_string(),
                extension_to_language,
                ..LspServerConfig::default()
            },
            server_name: Some(language.to_string()),
            scope: None,
            source: None,
        }
    }

    fn configs(
        entries: impl IntoIterator<Item = (&'static str, ScopedLspServerConfig)>,
    ) -> IndexMap<String, ScopedLspServerConfig> {
        entries
            .into_iter()
            .map(|(name, config)| (name.to_string(), config))
            .collect()
    }

    #[test]
    fn manager_initializes_extension_map_like_official() {
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([("rust", rust_config())]));

        assert_eq!(manager.server_count(), 1);
        assert!(manager.extensions().contains(".rs"));
        assert_eq!(
            manager
                .get_server_for_file("src/main.rs")
                .map(|server| server.name.clone()),
            Some("rust".to_string())
        );
        assert!(manager.get_server_for_file("README.md").is_none());
    }

    /// M1: CC iterates `Object.entries(serverConfigs)` in INSERTION order and
    /// `getServerForFile` takes `serverNames[0]` (`LSPServerManager.ts:89`,
    /// `:201`). Registering `zeta` first must win over `alpha`, which a
    /// `BTreeMap` would silently invert.
    #[test]
    fn same_extension_precedence_follows_insertion_order_not_sort_order() {
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([
            ("zeta", config_for_extension(".rs", "zeta")),
            ("alpha", config_for_extension(".rs", "alpha")),
        ]));

        assert_eq!(
            manager
                .get_server_for_file("src/main.rs")
                .map(|server| server.name.clone()),
            Some("zeta".to_string())
        );
        assert_eq!(
            manager
                .get_all_server_snapshots()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["zeta".to_string(), "alpha".to_string()]
        );
    }

    #[tokio::test]
    async fn manager_returns_none_for_unconfigured_file_without_starting_server() {
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([("rust", rust_config())]));

        let result = manager
            .send_request("README.md", "textDocument/hover", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(result, None);
        assert_eq!(
            manager.get_all_server_snapshots()["rust"].state,
            LspServerState::Stopped
        );
    }

    #[test]
    fn manager_skips_unsupported_lifecycle_fields_like_official_create_failure() {
        let mut invalid = rust_config();
        invalid.config.restart_on_crash = Some(true);
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([("rust", invalid)]));

        assert_eq!(manager.server_count(), 0);
        assert!(manager.get_server_for_file("src/main.rs").is_none());
    }

    /// M2 (faithful, not improved): CC registers the extension BEFORE
    /// `createLSPServerInstance` throws (`LSPServerManager.ts:107-120`), so a
    /// broken server poisons its extension and shadows a later valid one —
    /// `getServerForFile` resolves `serverNames[0]` to a name absent from
    /// `servers` and the tool answers "No LSP server available for file type".
    #[test]
    fn broken_server_poisons_its_extension_for_later_servers_like_official() {
        let mut broken = rust_config();
        broken.config.restart_on_crash = Some(true);
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([
            ("broken", broken),
            ("valid", config_for_extension(".rs", "valid")),
        ]));

        assert_eq!(manager.server_count(), 1);
        assert!(manager.extensions().contains(".rs"));
        assert!(
            manager.get_server_for_file("src/main.rs").is_none(),
            "the valid server must stay shadowed, exactly as in CC"
        );
    }

    /// The `command`/`extensionToLanguage` guards throw BEFORE the extension
    /// registration (`:92-104`), so those servers do not poison anything.
    #[test]
    fn missing_required_fields_skip_extension_registration_like_official() {
        let mut no_command = rust_config();
        no_command.config.command = String::new();
        let mut no_extensions = rust_config();
        no_extensions.config.extension_to_language.clear();
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([
            ("no-command", no_command),
            ("no-extensions", no_extensions),
            ("valid", config_for_extension(".rs", "valid")),
        ]));

        assert_eq!(manager.server_count(), 1);
        assert_eq!(
            manager
                .get_server_for_file("src/main.rs")
                .map(|server| server.name.clone()),
            Some("valid".to_string())
        );
    }

    #[tokio::test]
    async fn configured_server_failure_is_recorded_on_official_server_boundary() {
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([("rust", rust_config())]));

        let error = manager
            .send_request("src/main.rs", "textDocument/hover", serde_json::json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("failed to start") || error.contains("No such file"));
        let snapshot = manager.get_all_server_snapshots();
        assert_eq!(snapshot["rust"].state, LspServerState::Error);
        assert!(
            snapshot["rust"]
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("failed to start")
                    || message.contains("No such file"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_routes_fake_lsp_diagnostics_into_passive_registry() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::services::lsp::diagnostic_registry::reset_all_lsp_diagnostic_state();

        let script = write_fake_diagnostic_lsp_server();
        let mut extension_to_language = BTreeMap::new();
        extension_to_language.insert(".rs".to_string(), "rust".to_string());
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([(
            "fake",
            ScopedLspServerConfig {
                config: LspServerConfig {
                    command: "node".to_string(),
                    args: vec![script.display().to_string()],
                    extension_to_language,
                    ..LspServerConfig::default()
                },
                server_name: Some("fake".to_string()),
                scope: None,
                source: None,
            },
        )]));
        let registration =
            crate::services::lsp::passive_feedback::register_lsp_notification_handlers(&manager);
        assert_eq!(registration.total_servers, 1);
        assert_eq!(registration.success_count, 1);

        manager
            .open_file("src/main.rs", "fn main() {}".to_string())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let delivered = crate::services::lsp::diagnostic_registry::check_for_lsp_diagnostics();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].server_name, "fake");
        assert_eq!(
            delivered[0].files[0].diagnostics[0].message,
            "fake diagnostic"
        );

        manager.shutdown().await.unwrap();
        crate::services::lsp::diagnostic_registry::reset_all_lsp_diagnostic_state();
        let _ = fs::remove_file(script);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_restarts_crashed_lsp_server_on_next_request_like_official() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let script = write_fake_crashing_lsp_server();
        let state_file = std::env::temp_dir().join(format!(
            "cometix-fake-crashing-lsp-state-{}",
            uuid::Uuid::new_v4()
        ));
        let mut extension_to_language = BTreeMap::new();
        extension_to_language.insert(".rs".to_string(), "rust".to_string());
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([(
            "fake",
            ScopedLspServerConfig {
                config: LspServerConfig {
                    command: "node".to_string(),
                    args: vec![
                        script.display().to_string(),
                        state_file.display().to_string(),
                    ],
                    extension_to_language,
                    ..LspServerConfig::default()
                },
                server_name: Some("fake".to_string()),
                scope: None,
                source: None,
            },
        )]));

        let first = manager
            .send_request("src/main.rs", "textDocument/hover", serde_json::json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            first.contains("connection closed") || first.contains("request textDocument/hover"),
            "unexpected first error: {first}"
        );
        assert_eq!(
            manager.get_all_server_snapshots()["fake"].state,
            LspServerState::Error
        );

        let second = manager
            .send_request("src/main.rs", "textDocument/hover", serde_json::json!({}))
            .await
            .unwrap()
            .expect("server after restart");
        assert_eq!(second["contents"]["value"], "restarted");
        assert_eq!(
            manager.get_all_server_snapshots()["fake"].state,
            LspServerState::Running
        );

        manager.shutdown().await.unwrap();
        let _ = fs::remove_file(script);
        let _ = fs::remove_file(state_file);
    }

    /// M3: CC wires an `onCrash` callback (`LSPClient.ts:53`, `:165`;
    /// `LSPServerInstance.ts:121-125`) so the state flips to `error` AT CRASH
    /// TIME. The port used to poll `crash_error()` only after a request already
    /// failed, so a server that died between requests cost one extra failed
    /// tool call. This drives a crash with NO request in flight and asserts the
    /// instance is already `error` before anyone asks it anything.
    #[tokio::test(flavor = "current_thread")]
    async fn crash_between_requests_flips_state_at_crash_time_via_on_crash() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let script = write_fake_exit_on_notification_lsp_server();
        let mut extension_to_language = BTreeMap::new();
        extension_to_language.insert(".rs".to_string(), "rust".to_string());
        let manager = create_lsp_server_manager();
        manager.initialize_from_configs(configs([(
            "fake",
            ScopedLspServerConfig {
                config: LspServerConfig {
                    command: "node".to_string(),
                    args: vec![script.display().to_string()],
                    extension_to_language,
                    ..LspServerConfig::default()
                },
                server_name: Some("fake".to_string()),
                scope: None,
                source: None,
            },
        )]));

        let hover = manager
            .send_request("src/main.rs", "textDocument/hover", serde_json::json!({}))
            .await
            .unwrap()
            .expect("healthy server");
        assert_eq!(hover["contents"], "alive");
        assert_eq!(
            manager.get_all_server_snapshots()["fake"].state,
            LspServerState::Running
        );

        // A notification, not a request: nothing is awaiting a reply, so only
        // the crash callback can move the state.
        manager
            .save_file("src/main.rs")
            .await
            .expect("didSave is fire-and-forget");
        for _ in 0..200 {
            if manager.get_all_server_snapshots()["fake"].state == LspServerState::Error {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let snapshot = manager.get_all_server_snapshots();
        assert_eq!(
            snapshot["fake"].state,
            LspServerState::Error,
            "onCrash must flip the state without a request having failed"
        );
        assert!(
            snapshot["fake"]
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("connection closed"))
        );

        // CC stops `error`-state servers too (`:158-160`), and a dead
        // connection rejects the `shutdown` request, so `Promise.allSettled`
        // collects it and `shutdown()` throws. Same here — the point is that
        // the state is cleared either way.
        let _ = manager.shutdown().await;
        let _ = fs::remove_file(script);
    }

    /// H1: while a request is in flight, every read-side entry keeps working.
    /// Under the old `take()`/put-back shape the concurrent reader saw an empty
    /// slot and the tool answered "No LSP server available for file type: .rs".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_readers_see_the_manager_while_a_request_is_in_flight() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let script = write_fake_slow_lsp_server();
        let mut extension_to_language = BTreeMap::new();
        extension_to_language.insert(".rs".to_string(), "rust".to_string());
        let manager = Arc::new(create_lsp_server_manager());
        manager.initialize_from_configs(configs([(
            "fake",
            ScopedLspServerConfig {
                config: LspServerConfig {
                    command: "node".to_string(),
                    args: vec![script.display().to_string()],
                    extension_to_language,
                    ..LspServerConfig::default()
                },
                server_name: Some("fake".to_string()),
                scope: None,
                source: None,
            },
        )]));
        manager
            .open_file("src/main.rs", "fn main() {}".to_string())
            .await
            .unwrap();

        let in_flight = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move {
                manager
                    .send_request("src/main.rs", "textDocument/hover", serde_json::json!({}))
                    .await
            })
        };

        // The fake server holds the hover for 300ms; sample the read side while
        // it is outstanding.
        let mut samples = 0;
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            assert!(
                manager.is_file_open("src/main.rs"),
                "openedFiles must stay visible during a request"
            );
            assert_eq!(
                manager.get_all_server_snapshots()["fake"].state,
                LspServerState::Running,
                "server state must stay visible during a request"
            );
            assert!(
                manager.get_server_for_file("src/main.rs").is_some(),
                "extension routing must stay visible during a request"
            );
            samples += 1;
        }
        assert_eq!(samples, 10);

        let hover = in_flight.await.unwrap().unwrap().expect("hover result");
        assert_eq!(hover["contents"], "slow");

        manager.shutdown().await.unwrap();
        let _ = fs::remove_file(script);
    }

    fn write_fake_lsp_server_script(prefix: &str, body: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cometix-{prefix}-{unique}.mjs"));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    const FAKE_LSP_FRAMING: &str = r#"
let buffer = Buffer.alloc(0);
function send(message) {
  const body = Buffer.from(JSON.stringify(message));
  process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`);
  process.stdout.write(body);
}
process.stdin.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  while (true) {
    const headerEnd = buffer.indexOf('\r\n\r\n');
    if (headerEnd === -1) return;
    const header = buffer.slice(0, headerEnd).toString();
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) throw new Error('missing Content-Length');
    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    if (buffer.length < bodyStart + length) return;
    const body = buffer.slice(bodyStart, bodyStart + length);
    buffer = buffer.slice(bodyStart + length);
    handle(JSON.parse(body.toString()));
  }
});
"#;

    /// Answers hover normally, then exits on the next `didSave` NOTIFICATION —
    /// a crash with nothing awaiting a reply.
    fn write_fake_exit_on_notification_lsp_server() -> PathBuf {
        write_fake_lsp_server_script(
            "fake-exit-on-notification-lsp",
            &format!(
                r#"
function handle(message) {{
  if (message.method === 'initialize') {{
    send({{ jsonrpc: '2.0', id: message.id, result: {{ capabilities: {{ hoverProvider: true }} }} }});
  }} else if (message.method === 'textDocument/didSave') {{
    process.exit(1);
  }} else if (message.method === 'textDocument/hover') {{
    send({{ jsonrpc: '2.0', id: message.id, result: {{ contents: 'alive' }} }});
  }} else if (message.method === 'shutdown') {{
    send({{ jsonrpc: '2.0', id: message.id, result: null }});
  }} else if (message.method === 'exit') {{
    process.exit(0);
  }} else if (message.id !== undefined) {{
    send({{ jsonrpc: '2.0', id: message.id, result: null }});
  }}
}}
{FAKE_LSP_FRAMING}"#
            ),
        )
    }

    /// Holds `textDocument/hover` for 300ms so the read side can be sampled
    /// while a request is genuinely outstanding.
    fn write_fake_slow_lsp_server() -> PathBuf {
        write_fake_lsp_server_script(
            "fake-slow-lsp",
            &format!(
                r#"
function handle(message) {{
  if (message.method === 'initialize') {{
    send({{ jsonrpc: '2.0', id: message.id, result: {{ capabilities: {{ hoverProvider: true }} }} }});
  }} else if (message.method === 'textDocument/hover') {{
    setTimeout(() => send({{ jsonrpc: '2.0', id: message.id, result: {{ contents: 'slow' }} }}), 300);
  }} else if (message.method === 'shutdown') {{
    send({{ jsonrpc: '2.0', id: message.id, result: null }});
  }} else if (message.method === 'exit') {{
    process.exit(0);
  }} else if (message.id !== undefined) {{
    send({{ jsonrpc: '2.0', id: message.id, result: null }});
  }}
}}
{FAKE_LSP_FRAMING}"#
            ),
        )
    }

    fn write_fake_diagnostic_lsp_server() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cometix-fake-diagnostic-lsp-{unique}.mjs"));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(
            br#"
let buffer = Buffer.alloc(0);
function send(message) {
  const body = Buffer.from(JSON.stringify(message));
  process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`);
  process.stdout.write(body);
}
function handle(message) {
  if (message.method === 'initialize') {
    send({ jsonrpc: '2.0', id: message.id, result: { capabilities: { textDocumentSync: 1 } } });
  } else if (message.method === 'initialized') {
    send({ jsonrpc: '2.0', method: 'textDocument/publishDiagnostics', params: {
      uri: 'file:///tmp/fake.rs',
      diagnostics: [{
        message: 'fake diagnostic',
        severity: 1,
        range: { start: { line: 0, character: 0 }, end: { line: 0, character: 4 } },
        source: 'fake-lsp',
        code: 'Efake'
      }]
    }});
  } else if (message.method === 'shutdown') {
    send({ jsonrpc: '2.0', id: message.id, result: null });
  } else if (message.method === 'exit') {
    process.exit(0);
  } else if (message.id !== undefined) {
    send({ jsonrpc: '2.0', id: message.id, result: null });
  }
}
process.stdin.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  while (true) {
    const headerEnd = buffer.indexOf('\r\n\r\n');
    if (headerEnd === -1) return;
    const header = buffer.slice(0, headerEnd).toString();
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) throw new Error('missing Content-Length');
    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    if (buffer.length < bodyStart + length) return;
    const body = buffer.slice(bodyStart, bodyStart + length);
    buffer = buffer.slice(bodyStart + length);
    handle(JSON.parse(body.toString()));
  }
});
"#,
        )
        .unwrap();
        path
    }

    fn write_fake_crashing_lsp_server() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cometix-fake-crashing-lsp-{unique}.mjs"));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(
            br#"
import fs from 'fs';
const stateFile = process.argv[2];
let buffer = Buffer.alloc(0);
function send(message) {
  const body = Buffer.from(JSON.stringify(message));
  process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`);
  process.stdout.write(body);
}
function handle(message) {
  if (message.method === 'initialize') {
    send({ jsonrpc: '2.0', id: message.id, result: { capabilities: { hoverProvider: true } } });
  } else if (message.method === 'shutdown') {
    send({ jsonrpc: '2.0', id: message.id, result: null });
  } else if (message.method === 'exit') {
    process.exit(0);
  } else if (message.method === 'textDocument/hover') {
    if (!fs.existsSync(stateFile)) {
      fs.writeFileSync(stateFile, 'crashed');
      process.exit(1);
    }
    send({ jsonrpc: '2.0', id: message.id, result: { contents: { kind: 'plaintext', value: 'restarted' } } });
  } else if (message.id !== undefined) {
    send({ jsonrpc: '2.0', id: message.id, result: null });
  }
}
process.stdin.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  while (true) {
    const headerEnd = buffer.indexOf('\r\n\r\n');
    if (headerEnd === -1) return;
    const header = buffer.slice(0, headerEnd).toString();
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) throw new Error('missing Content-Length');
    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    if (buffer.length < bodyStart + length) return;
    const body = buffer.slice(bodyStart, bodyStart + length);
    buffer = buffer.slice(bodyStart + length);
    handle(JSON.parse(body.toString()));
  }
});
"#,
        )
        .unwrap();
        path
    }
}
