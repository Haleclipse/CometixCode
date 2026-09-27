//! MCP connection manager: the mounted component plus its context boundary.
//! Maps to: CC `services/mcp/MCPConnectionManager.tsx`.
//!
//! CC mounts `<MCPConnectionManager …>` inside `screens/REPL.tsx:6134-6138`,
//! and the connection work is an effect inside `useManageMCPConnections`
//! (`:770-838`). That placement is what orders it: the effect cannot run before
//! the component mounts, which cannot happen before `AppStateProvider` has
//! created the store and bound its change pipeline.
//!
//! Cometix used to spawn the same work from `main.rs` ahead of `render_loop()`,
//! onto a multi-threaded runtime, with nothing ordering it against
//! `bind_on_change`. An MCP write could therefore land before adoption and be
//! silent where CC's would notify. These are mounted-effect writes at the
//! source, not pre-mount writes, so the fix is to put them where the source
//! puts them rather than to add a barrier CC has no counterpart for
//! (P5 G10).

use iocraft::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::services::mcp::config::all_configured_mcp_servers_readonly;
use crate::services::mcp::types::ScopedMcpServerConfig;
use crate::services::mcp::types::{McpServerConnectionType, McpServerSnapshot};
use crate::state::app_state_store::McpWriter;
use crate::state::store::AppStore;
use crate::utils::config::{ProjectConfig, load_global_config, normalize_project_path};

/// Maps to: CC `MCPConnectionManagerProps` (`:47-51`) — `dynamicMcpConfig` and
/// `isStrictMcpConfig` are props at the source too, carried here inside
/// `McpStartupConfig`.
#[derive(Default, Props)]
pub struct McpConnectionManagerProps<'a> {
    /// Maps to: CC `MCPConnectionManagerProps.children` (`:48`).
    pub children: Vec<AnyElement<'a>>,
    /// The adopted store. `Option` only because iocraft `Props` must derive
    /// `Default`; a missing store makes the effect a no-op rather than a panic,
    /// since this component renders nothing a caller could be waiting on.
    pub app_store: Option<AppStore>,
    pub mcp_startup: Option<Arc<crate::main::McpStartupConfig>>,
}

/// Maps to: CC `MCPConnectionManager` (`:55-73`).
///
/// Owns the connection effect AND publishes the context its two hooks read,
/// wrapping its children exactly as `<MCPConnectionContext.Provider>` does
/// (`:69-73`).
///
/// Until 2026-08-04 this component existed but was never mounted: the effect
/// was called straight from `screens/repl.rs`, and the REPL comment claimed a
/// mapping to `REPL.tsx:6134-6138` that the code did not implement. With no
/// manager in the tree there was no context to read, which is precisely why
/// consumers had invented `use_mcp` — a store-derived writer with no CC
/// counterpart. Mounting it is what makes the source's hooks possible.
/// A raw `Component`, not a `#[component]` function, for the same reason
/// iocraft's own `ContextProvider` is: a pass-through parent must hand its
/// children to `update_children` by `iter_mut()`. A `#[component]` fn can only
/// move them into the element it returns, i.e. `drain()` — which empties the
/// props on the first render and leaves every later frame blank.
#[derive(Default)]
pub struct McpConnectionManager;

/// How many times a manager has been mounted in this process — the REPL
/// tests assert a dialog coming and going does not remount it.
#[cfg(test)]
pub(crate) static MOUNTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Component for McpConnectionManager {
    type Props<'a> = McpConnectionManagerProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        #[cfg(test)]
        MOUNTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        let mut hooks = hooks.with_context_stack(updater.component_context_stack());
        // CC `:59-62` — the connection work.
        super::use_manage_mcp_connections::use_manage_mcp_connections(
            &mut hooks,
            props.app_store.clone(),
            props.mcp_startup.as_ref().map(Arc::clone),
        );
        // CC `:63-66` — `useMemo(() => ({reconnectMcpServer, toggleMcpServer}), …)`.
        // The `use_state` initializer runs once, which is what keeps the
        // published value stable across renders.
        let context = hooks
            .use_state({
                let app_store = props.app_store.clone();
                move || McpConnectionContext::new(app_store)
            })
            .read()
            .clone();

        // CC `:69-73`. Published UNCONDITIONALLY, as at the source: being
        // inside the manager is the whole precondition its two hooks check. A
        // missing store makes the callbacks no-ops, which is a different thing
        // from "the manager is not mounted" and must not be reported as one.
        updater.set_transparent_layout(true);
        updater.update_children(
            props.children.iter_mut(),
            Some(Context::owned(context).borrow()),
        );
    }
}

/// Maps to: CC `MCPConnectionContextValue` (`:15-24`) — the memoized
/// `{ reconnectMcpServer, toggleMcpServer }` pair the manager publishes.
#[derive(Clone)]
pub struct McpConnectionContext {
    /// `None` only when the manager was mounted without a store — the
    /// `Props: Default` artifact, not a source state. The callbacks then
    /// no-op, exactly as CC's would with an empty client list.
    runtime_mcp: Option<McpWriter>,
}

impl McpConnectionContext {
    fn new(store: Option<AppStore>) -> Self {
        Self {
            runtime_mcp: store.map(McpWriter::new),
        }
    }
}

/// Maps to: CC `useMcpReconnect()` context error string.
pub const USE_MCP_RECONNECT_CONTEXT_ERROR: &str =
    "useMcpReconnect must be used within MCPConnectionManager";
/// Maps to: CC `useMcpToggleEnabled()` context error string.
pub const USE_MCP_TOGGLE_ENABLED_CONTEXT_ERROR: &str =
    "useMcpToggleEnabled must be used within MCPConnectionManager";

/// The single function CC's `useMcpReconnect()` returns (`:36`).
///
/// A newtype rather than a bare closure because the callback is `async`; Rust
/// has no ergonomic first-class async closure, and returning the whole context
/// would hand a reconnect-only consumer the toggle callback the source withheld.
#[derive(Clone)]
pub struct ReconnectMcpServer(McpConnectionContext);

impl ReconnectMcpServer {
    /// Maps to: CC `reconnectMcpServer(serverName)`.
    pub async fn call(&self, server_name: &str) -> Option<McpServerSnapshot> {
        reconnect_mcp_server_from_manager(server_name, self.0.runtime_mcp.clone()?).await
    }
}

/// The single function CC's `useMcpToggleEnabled()` returns (`:45`).
#[derive(Clone)]
pub struct ToggleMcpServer(McpConnectionContext);

impl ToggleMcpServer {
    /// Maps to: CC `toggleMcpServer(serverName)` — `Err` is the source's throw.
    pub async fn call(&self, server_name: &str) -> anyhow::Result<Option<McpServerSnapshot>> {
        let Some(runtime_mcp) = self.0.runtime_mcp.clone() else {
            return Ok(None);
        };
        toggle_mcp_server_from_manager(server_name, runtime_mcp).await
    }
}

/// Maps to: CC `useMcpReconnect()` (`:30-37`) — throws outside the manager.
pub fn use_mcp_reconnect(hooks: &mut Hooks) -> ReconnectMcpServer {
    ReconnectMcpServer(
        hooks
            .try_use_context::<McpConnectionContext>()
            .expect(USE_MCP_RECONNECT_CONTEXT_ERROR)
            .clone(),
    )
}

/// Maps to: CC `useMcpToggleEnabled()` (`:39-46`) — throws outside the manager.
pub fn use_mcp_toggle_enabled(hooks: &mut Hooks) -> ToggleMcpServer {
    ToggleMcpServer(
        hooks
            .try_use_context::<McpConnectionContext>()
            .expect(USE_MCP_TOGGLE_ENABLED_CONTEXT_ERROR)
            .clone(),
    )
}

/// Maps to: CC `MCPConnectionManager` reconnect context callback resolving the
/// active project config before delegating to `useManageMCPConnections`.
/// Reads `getGlobalConfig()`-equivalent cached config directly; globalConfig
/// is not part of CC AppState.
pub fn project_config_from_global_config() -> ProjectConfig {
    std::env::current_dir()
        .ok()
        .map(|cwd| {
            let key = normalize_project_path(&cwd.to_string_lossy());
            crate::utils::config::load_global_config()
                .projects
                .get(&key)
                .cloned()
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Maps to: CC `MCPConnectionManager` callback using the configured server map
/// from `useManageMCPConnections(...)` when the AppState row lacks config.
pub fn configured_server_from_global_config(server_name: &str) -> Option<ScopedMcpServerConfig> {
    let project_config = project_config_from_global_config();
    all_configured_mcp_servers_readonly(
        &crate::utils::config::load_global_config(),
        &project_config,
    )
    .remove(server_name)
}

fn resolve_server_config(
    server_name: &str,
    server: &McpServerSnapshot,
) -> Option<ScopedMcpServerConfig> {
    server
        .config
        .clone()
        .or_else(|| configured_server_from_global_config(server_name))
}

/// Maps to: CC `useMcpReconnect()` returning `context.reconnectMcpServer`.
pub async fn reconnect_mcp_server_from_manager(
    server_name: &str,
    runtime_mcp: McpWriter,
) -> Option<McpServerSnapshot> {
    let current = runtime_mcp.current();
    let runtime_server = current
        .clients
        .iter()
        .find(|server| server.client.name == server_name)?;
    let config = resolve_server_config(server_name, runtime_server)?;

    runtime_mcp.apply_client_update(
        crate::services::mcp::client::McpConnectionDiscovery::pending_with_config(
            server_name,
            &config,
        )
        .server,
    );
    let server = crate::services::mcp::use_manage_mcp_connections::reconnect_mcp_server_once(
        server_name,
        &config,
    )
    .await;
    runtime_mcp.apply_server_update(server.clone());
    Some(server.server)
}

/// Maps to: CC `useMcpToggleEnabled()` returning `context.toggleMcpServer`.
///
/// PROPAGATES the failure, because CC's callback throws and its menus wrap the
/// call in `try/catch` (`MCPStdioServerMenu.tsx:58-70`). Swallowing it into a
/// `Failed` snapshot — which this did until 2026-08-04 — loses the distinction
/// the source depends on:
///
/// - `toggle_mcp_server_once` errs on server-not-found and on a failed settings
///   write. Those are CC's `throw`, and the menu must report them.
/// - It returns `Ok` for a successful disable, and for a successful ENABLE whose
///   subsequent connect then fails. The second case is not a throw at the
///   source, so the menu must stay silent and just return to the list.
///
/// Collapsed into "status == Failed", a server that enables correctly but
/// cannot reach its transport would be reported as "Failed to enable".
pub async fn toggle_mcp_server_from_manager(
    server_name: &str,
    runtime_mcp: McpWriter,
) -> anyhow::Result<Option<McpServerSnapshot>> {
    let current = runtime_mcp.current();
    let Some(runtime_server) = current
        .clients
        .iter()
        .find(|server| server.client.name == server_name)
    else {
        return Ok(None);
    };
    let Some(config) = resolve_server_config(server_name, runtime_server) else {
        return Ok(None);
    };

    if runtime_server.client.status == McpServerConnectionType::Disabled {
        runtime_mcp.apply_client_update(
            crate::services::mcp::client::McpConnectionDiscovery::pending_with_config(
                server_name,
                &config,
            )
            .server,
        );
    }

    match crate::services::mcp::use_manage_mcp_connections::toggle_mcp_server_once(
        server_name,
        &current,
        &config,
    )
    .await
    {
        Ok(server) => {
            runtime_mcp.apply_server_update(server.clone());
            Ok(Some(server.server))
        }
        Err(error) => {
            // The pending row installed above must not be left hanging when the
            // toggle threw; the source's catch path leaves AppState as the
            // manager last wrote it, which for a throw is the failed client.
            runtime_mcp.apply_server_update(
                crate::services::mcp::client::McpConnectionDiscovery::failed(
                    server_name,
                    &config,
                    error.to_string(),
                )
                .server,
            );
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_connection_manager_context_errors_match_official_copy() {
        assert_eq!(
            USE_MCP_RECONNECT_CONTEXT_ERROR,
            "useMcpReconnect must be used within MCPConnectionManager"
        );
        assert_eq!(
            USE_MCP_TOGGLE_ENABLED_CONTEXT_ERROR,
            "useMcpToggleEnabled must be used within MCPConnectionManager"
        );
    }
}
