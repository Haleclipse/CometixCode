//! MCP connection manager boundary.
//! Maps to: CC `services/mcp/useManageMCPConnections.ts`.
//!
//! The React hook in CC reconciles configured MCP servers with AppState.mcp,
//! connects clients, refreshes tools/prompts/resources, and exposes reconnect
//! callbacks. Cometix keeps the same service boundary as async functions so the
//! iocraft App root can drive it without putting transport logic in components
//! or slash commands.

use super::channel_notification::{
    ChannelGateResult, ChannelRuntimeGateContext, channel_gate_context_from_readonly_runtime,
    enqueue_channel_message_notification, gate_channel_server_from_context,
};
use super::channel_permissions::ChannelPermissionCallbacks;
use super::client::{
    McpConnectionDiscovery, clear_server_cache, get_mcp_tools_commands_and_resources,
    reconnect_mcp_server_impl, refresh_mcp_prompts_for_client, refresh_mcp_resources_for_client,
    refresh_mcp_tools_for_client,
};
use super::config::{is_mcp_server_disabled, set_mcp_server_enabled};
use super::types::{
    McpPromptSnapshot, McpServerConnectionType, McpServerSnapshot, McpToolSnapshot,
    ScopedMcpServerConfig, ServerResource, Transport,
};
use crate::state::app_state_store::{McpState, McpWriter};
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Maps to: CC `useManageMCPConnections.ts:95#getErrorKey`.
fn get_error_key(error: &crate::types::plugin::PluginError) -> String {
    let value = serde_json::to_value(error).expect("PluginError serializes");
    let plugin = value
        .get("plugin")
        .map(crate::utils::zod::js_string)
        .unwrap_or_else(|| "no-plugin".to_owned());
    format!(
        "{}:{}:{plugin}",
        value["type"].as_str().unwrap(),
        error.source()
    )
}

/// Maps to: CC `useManageMCPConnections.ts:103#addErrorsToAppState`.
fn add_errors_to_app_state(
    store: &crate::state::store::AppStore,
    errors: Vec<crate::types::plugin::PluginError>,
) {
    if errors.is_empty() {
        return;
    }
    store.set_state(|previous| {
        let existing = previous
            .plugins
            .errors
            .iter()
            .map(get_error_key)
            .collect::<std::collections::HashSet<_>>();
        // The source deduplicates against previous state, not within newErrors.
        let unique = errors
            .into_iter()
            .filter(|error| !existing.contains(&get_error_key(error)))
            .collect::<Vec<_>>();
        if unique.is_empty() {
            return crate::state::store::UpdateDecision::Same(());
        }
        let mut next = (**previous).clone();
        std::sync::Arc::make_mut(&mut next.plugins)
            .errors
            .extend(unique);
        crate::state::store::UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `services/mcp/useManageMCPConnections.ts#MAX_RECONNECT_ATTEMPTS`.
pub const MAX_RECONNECT_ATTEMPTS: u32 = 5;
#[derive(Debug)]
struct ReconnectTaskEntry {
    handle: JoinHandle<()>,
}

static RECONNECT_TASKS: LazyLock<std::sync::Mutex<HashMap<String, ReconnectTaskEntry>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
/// Maps to: CC `services/mcp/useManageMCPConnections.ts#INITIAL_BACKOFF_MS`.
pub const INITIAL_BACKOFF_MS: u64 = 1_000;
/// Maps to: CC `services/mcp/useManageMCPConnections.ts#MAX_BACKOFF_MS`.
pub const MAX_BACKOFF_MS: u64 = 30_000;

/// Maps to: CC `services/mcp/useManageMCPConnections.ts#getTransportDisplayName`.
/// Maps to CC `useManageMCPConnections.ts` creating
/// `AppState.channelPermissionCallbacks` only when channels and
/// `isChannelPermissionRelayEnabled()` are both enabled. The callbacks live in
/// `AppState.channel_permission_callbacks`; this is the startup seed.
pub fn channel_permission_callbacks_from_official_gates()
-> Option<crate::services::mcp::channel_permissions::ChannelPermissionCallbacks> {
    let channels_enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::ChannelsEnabled,
    );
    let relay_enabled =
        crate::services::mcp::channel_permissions::is_channel_permission_relay_enabled();
    (channels_enabled && relay_enabled)
        .then(crate::services::mcp::channel_permissions::ChannelPermissionCallbacks::default)
}

pub fn get_transport_display_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Http => "HTTP",
        Transport::Ws | Transport::WsIde => "WebSocket",
        _ => "SSE",
    }
}

/// Maps to: CC `useManageMCPConnections.ts` automatic reconnect guard:
/// `configType !== 'stdio' && configType !== 'sdk'`.
pub fn transport_supports_automatic_reconnect(transport: Transport) -> bool {
    !matches!(transport, Transport::Stdio | Transport::Sdk)
}

/// Maps to: CC `INITIAL_BACKOFF_MS * Math.pow(2, attempt - 1)` capped at
/// `MAX_BACKOFF_MS`.
pub fn reconnect_backoff_ms(attempt: u32) -> u64 {
    let exponent = attempt.saturating_sub(1).min(63);
    INITIAL_BACKOFF_MS
        .saturating_mul(1u64 << exponent)
        .min(MAX_BACKOFF_MS)
}

/// Maps to: CC `updateServer({ ...client, type: 'pending', reconnectAttempt,
/// maxReconnectAttempts })` inside `reconnectWithBackoff`.
pub fn pending_reconnect_server_update(
    server: &McpServerSnapshot,
    attempt: u32,
) -> McpServerSnapshot {
    let mut updated = server.clone();
    updated.client.status = McpServerConnectionType::Pending;
    updated.client.reconnect_attempt = Some(attempt);
    updated.client.max_reconnect_attempts = Some(MAX_RECONNECT_ATTEMPTS);
    updated.client.error = None;
    updated
}

/// Maps to: CC `updateServer({ ...client, type: 'failed' })` reconnect final
/// failure branch. `updateServer` clears tools/commands/resources for failed
/// clients, so the aggregate runtime snapshot clears capabilities here too.
pub fn failed_reconnect_server_update(server: &McpServerSnapshot) -> McpServerSnapshot {
    let mut updated = server.clone();
    updated.client.status = McpServerConnectionType::Failed;
    updated.client.reconnect_attempt = None;
    updated.client.max_reconnect_attempts = None;
    updated.client.error = None;
    updated.tools.clear();
    updated.prompts.clear();
    updated.resources.clear();
    updated
}

/// Maps to: CC `MCPRemoteServerMenu.tsx#handleClearAuth` AppState update:
/// mark the regular remote client failed and remove that server's tools,
/// prompt commands, and resources after revoking tokens and clearing cache.
pub fn clear_authentication_server_update(server: &McpServerSnapshot) -> McpServerSnapshot {
    let mut updated = server.clone();
    updated.client.status = McpServerConnectionType::Failed;
    updated.client.reconnect_attempt = None;
    updated.client.max_reconnect_attempts = None;
    updated.client.error = None;
    updated.tools.clear();
    updated.prompts.clear();
    updated.resources.clear();
    updated
}

/// Maps to CC `MCPRemoteServerMenu.tsx#handleAuthenticate` result copy.
pub fn remote_authentication_result_message(
    status: McpServerConnectionType,
    server_name: &str,
    was_effectively_authenticated: bool,
) -> String {
    match status {
        McpServerConnectionType::Connected => {
            if was_effectively_authenticated {
                format!("Authentication successful. Reconnected to {server_name}.")
            } else {
                format!("Authentication successful. Connected to {server_name}.")
            }
        }
        McpServerConnectionType::NeedsAuth => "Authentication successful, but server still requires authentication. You may need to manually restart Claude Code.".to_string(),
        _ => "Authentication successful, but server reconnection failed. You may need to manually restart Claude Code for the changes to take effect.".to_string(),
    }
}

/// Maps to CC `MCPRemoteServerMenu.tsx#handleAuthenticate` side-effect order:
/// preserve step-up OAuth state for re-auth, run the MCP OAuth service flow,
/// then reconnect the server through the MCP connection manager.
pub async fn authenticate_remote_mcp_server_once(
    server_name: &str,
    config: &ScopedMcpServerConfig,
    was_authenticated: bool,
    was_effectively_authenticated: bool,
    on_authorization_url: Option<super::auth::McpAuthorizationUrlCallback>,
    on_waiting_for_callback: Option<super::auth::McpWaitingForCallbackCallback>,
    abort_signal: Option<super::auth::McpOAuthAbortSignal>,
) -> anyhow::Result<(McpConnectionDiscovery, String)> {
    if was_authenticated {
        super::auth::revoke_server_tokens(server_name, config, true).await?;
    }

    super::auth::perform_mcp_oauth_flow(
        server_name,
        config,
        super::auth::McpOAuthFlowOptions {
            skip_browser_open: false,
            on_authorization_url,
            on_waiting_for_callback,
            abort_signal,
        },
    )
    .await?;

    let updated = reconnect_mcp_server_once(server_name, config).await;
    let message = remote_authentication_result_message(
        updated.server.client.status,
        server_name,
        was_effectively_authenticated,
    );
    Ok((updated, message))
}

/// Maps to CC `MCPRemoteServerMenu.tsx#handleClaudeAIClearAuthComplete` AppState
/// update: mark the Claude.ai proxy client as needs-auth and remove tools,
/// prompt commands, and resources.
pub fn claude_ai_clear_authentication_server_update(
    server: &McpServerSnapshot,
) -> McpServerSnapshot {
    let mut updated = server.clone();
    updated.client.status = McpServerConnectionType::NeedsAuth;
    updated.client.reconnect_attempt = None;
    updated.client.max_reconnect_attempts = None;
    updated.client.error = None;
    updated.tools.clear();
    updated.prompts.clear();
    updated.resources.clear();
    updated
}

/// Maps to: CC `updateServer({ ...client, tools: newTools })` in the
/// `tools/list_changed` notification handler.
pub fn tools_list_changed_server_update(
    server: &McpServerSnapshot,
    tools: Vec<McpToolSnapshot>,
) -> McpServerSnapshot {
    let mut updated = server.clone();
    updated.tools = tools;
    updated
}

/// Maps to: CC `updateServer({ ...client, commands: [...] })` in the
/// `prompts/list_changed` notification handler. Cometix stores MCP prompt
/// commands in `McpServerSnapshot.prompts`.
pub fn prompts_list_changed_server_update(
    server: &McpServerSnapshot,
    prompts: Vec<McpPromptSnapshot>,
) -> McpServerSnapshot {
    let mut updated = server.clone();
    updated.prompts = prompts;
    updated
}

/// Maps to: CC `updateServer({ ...client, resources: newResources })` in the
/// non-`MCP_SKILLS` `resources/list_changed` branch.
pub fn resources_list_changed_server_update(
    server: &McpServerSnapshot,
    resources: Vec<ServerResource>,
) -> McpServerSnapshot {
    let mut updated = server.clone();
    updated.resources = resources;
    updated
}

/// Maps to: CC `client.client.onclose` branch ordering: disabled servers stop,
/// stdio/sdk are marked failed, and remote transports reconnect with backoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClosedServerReconnectDecision {
    SkipDisabled,
    MarkFailed,
    StartAutomaticReconnect,
}

/// Maps to: CC `client.client.onclose` reconnect decision.
pub fn closed_server_reconnect_decision(
    transport: Transport,
    is_disabled: bool,
) -> ClosedServerReconnectDecision {
    if is_disabled {
        ClosedServerReconnectDecision::SkipDisabled
    } else if transport_supports_automatic_reconnect(transport) {
        ClosedServerReconnectDecision::StartAutomaticReconnect
    } else {
        ClosedServerReconnectDecision::MarkFailed
    }
}

fn reconnect_base_server(
    name: &str,
    config: &ScopedMcpServerConfig,
    runtime_mcp: McpWriter,
) -> McpServerSnapshot {
    runtime_mcp
        .current()
        .clients
        .into_iter()
        .find(|server| server.client.name == name)
        .unwrap_or_else(|| McpConnectionDiscovery::pending_with_config(name, config).server)
}

/// Maps to: CC `reconnectTimersRef.current.get(name)` cancellation before
/// manual reconnect, disable, and replacement reconnect attempts.
pub fn cancel_pending_mcp_reconnect(name: &str) {
    if let Some(task) = RECONNECT_TASKS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(name)
    {
        task.handle.abort();
    }
}

/// Maps to: CC `client.client.onclose` branch that either marks stdio/sdk
/// clients failed or starts `reconnectWithBackoff` for remote transports.
pub async fn handle_mcp_server_closed_event(
    name: String,
    config: ScopedMcpServerConfig,
    runtime_mcp: McpWriter,
) {
    clear_server_cache(&name, None).await;
    match closed_server_reconnect_decision(config.transport, is_mcp_server_disabled(&name)) {
        ClosedServerReconnectDecision::SkipDisabled => {}
        ClosedServerReconnectDecision::MarkFailed => {
            let base = reconnect_base_server(&name, &config, runtime_mcp.clone());
            runtime_mcp.apply_server_update(failed_reconnect_server_update(&base));
        }
        ClosedServerReconnectDecision::StartAutomaticReconnect => {
            start_automatic_mcp_reconnect(name, config, runtime_mcp).await;
        }
    }
}

/// Resolves the full-client base a list_changed update rides on. CC's
/// notification handler closure captures the connection-time client and sends
/// `updateServer({ ...client, <dimension> })` — that captured snapshot is
/// always `type: 'connected'`, so the flush replaces the record with a
/// connected client even if it mutated (pending/disabled) in the meantime,
/// and the flush's disabled/failed clearing rule
/// (`useManageMCPConnections.ts:232-243`) is unreachable on this path. The
/// Rust routing layer carries only the server name, so the base is resolved
/// from the store at apply time with the status FORCED back to Connected to
/// preserve both properties; a missing client is resynthesized likewise so
/// the append-when-missing semantics (:250-253) survive the
/// stale-notification race. Carrying the true connection-time snapshot
/// end-to-end remains an explicit seam for the MCP producer parity batch.
fn resolve_list_changed_client_base(name: &str, runtime_mcp: &McpWriter) -> McpServerSnapshot {
    let mut server = runtime_mcp
        .current()
        .clients
        .into_iter()
        .find(|server| server.client.name == name)
        .unwrap_or_else(|| McpConnectionDiscovery::pending(name).server);
    server.client.status = McpServerConnectionType::Connected;
    server
}

/// Maps to: CC `ToolListChangedNotificationSchema` handler in
/// `services/mcp/useManageMCPConnections.ts#onConnectionAttempt`
/// (`updateServer({ ...client, tools: newTools })`, :656).
pub async fn handle_mcp_tools_list_changed_event(name: String, runtime_mcp: McpWriter) {
    match refresh_mcp_tools_for_client(&name).await {
        Ok(tools) => {
            let mut update = resolve_list_changed_client_base(&name, &runtime_mcp);
            update.tools = tools;
            runtime_mcp.apply_tools_list_changed(update);
        }
        Err(error) => {
            tracing::warn!(server = %name, error = %error, "failed to refresh MCP tools after list_changed notification")
        }
    }
}

/// Maps to: CC `PromptListChangedNotificationSchema` handler in
/// `services/mcp/useManageMCPConnections.ts#onConnectionAttempt`
/// (`updateServer({ ...client, commands: [...] })`, :688-691).
pub async fn handle_mcp_prompts_list_changed_event(name: String, runtime_mcp: McpWriter) {
    match refresh_mcp_prompts_for_client(&name).await {
        Ok(prompts) => {
            let mut update = resolve_list_changed_client_base(&name, &runtime_mcp);
            update.prompts = prompts;
            runtime_mcp.apply_prompts_list_changed(update);
        }
        Err(error) => {
            tracing::warn!(server = %name, error = %error, "failed to refresh MCP prompts after list_changed notification")
        }
    }
}

/// Maps to: CC `ResourceListChangedNotificationSchema` handler in the
/// non-`MCP_SKILLS` branch (`updateServer({ ...client, resources })`, :741).
/// MCP skill refresh/index invalidation remains tied to the separate Skill
/// runtime parity item.
pub async fn handle_mcp_resources_list_changed_event(name: String, runtime_mcp: McpWriter) {
    match refresh_mcp_resources_for_client(&name).await {
        Ok(resources) => {
            let mut update = resolve_list_changed_client_base(&name, &runtime_mcp);
            update.resources = resources;
            runtime_mcp.apply_resources_list_changed(update);
        }
        Err(error) => {
            tracing::warn!(server = %name, error = %error, "failed to refresh MCP resources after list_changed notification")
        }
    }
}

/// Maps to: CC `useManageMCPConnections.ts` channel notification handler after
/// `gateChannelServer(...)` returns `register`.
pub fn handle_channel_message_notification_with_context(
    server_name: &str,
    content: &str,
    meta: Option<&BTreeMap<String, String>>,
    has_channel_capability: bool,
    plugin_source: Option<&str>,
    context: &ChannelRuntimeGateContext,
) -> ChannelGateResult {
    let gate = gate_channel_server_from_context(
        server_name,
        has_channel_capability,
        context.channels_enabled,
        context.has_claude_ai_oauth,
        context.subscription.as_deref(),
        context.policy_channels_enabled,
        &context.session_channels,
        plugin_source,
        &context.allowlist,
    );
    if gate == ChannelGateResult::Register {
        enqueue_channel_message_notification(server_name, content, meta);
    }
    gate
}

/// Maps to: CC `gateChannelServer(...)` runtime reads and enqueue side effect
/// inside `services/mcp/useManageMCPConnections.ts#onConnectionAttempt`.
pub fn handle_channel_message_notification_event(
    server_name: &str,
    content: &str,
    meta: Option<&BTreeMap<String, String>>,
    has_channel_capability: bool,
    plugin_source: Option<&str>,
) -> ChannelGateResult {
    let policy_settings = crate::utils::settings::get_settings_for_source(
        crate::utils::settings::constants::SettingSource::Policy,
    );
    let context = channel_gate_context_from_readonly_runtime(policy_settings.as_ref());
    handle_channel_message_notification_with_context(
        server_name,
        content,
        meta,
        has_channel_capability,
        plugin_source,
        &context,
    )
}

/// Maps to: CC `useManageMCPConnections.ts` channel permission notification
/// handler registered under `CHANNEL_PERMISSION_METHOD`.
pub fn handle_channel_permission_notification_event(
    callbacks: Option<&crate::services::mcp::channel_permissions::ChannelPermissionCallbacks>,
    server_name: &str,
    request_id: &str,
    behavior: crate::services::mcp::channel_permissions::ChannelPermissionBehavior,
    has_permission_capability: bool,
) -> Option<bool> {
    if !has_permission_capability {
        tracing::debug!(server = %server_name, request_id = %request_id, "MCP channel permission notification skipped: missing capability");
        return None;
    }
    Some(
        crate::services::mcp::channel_permissions::handle_channel_permission_notification(
            callbacks,
            request_id,
            behavior,
            server_name,
        ),
    )
}

// ---------------------------------------------------------------------------
// onConnectionAttempt handlers (CC useManageMCPConnections.ts)
//
// Claude Code registers `onclose` / `setNotificationHandler` closures inside
// `onConnectionAttempt` that call `updateServer` / `setAppState` directly.
// Cometix keeps that ownership here: App mounts [`OnConnectionAttemptHandlers`],
// and the rmcp client handler invokes the `emit_*` entry points below.
// IDE `selection_changed` is REPL-owned (CC `useIdeSelection`): the REPL
// threads its per-connection selection sender through this registry via
// [`set_ide_selection_sink`]; `replace_cached_client` clones it into each new
// "ide" connection entry (CC `client.setNotificationHandler(...)` per client).
// ---------------------------------------------------------------------------

use crate::services::mcp::elicitation_handler::ElicitationRequestEvent;
use std::collections::VecDeque;
use std::sync::{Mutex as StdMutex, OnceLock};
use tokio::sync::Mutex as AsyncMutex;

static CONNECTION_ATTEMPT_HANDLERS: OnceLock<StdMutex<ConnectionAttemptRegistry>> = OnceLock::new();

/// Session-scoped registrations threaded from the retained tree into the MCP
/// connection layer. One existing slot; not an AppStore writer.
#[derive(Default)]
struct ConnectionAttemptRegistry {
    handlers: Option<OnConnectionAttemptHandlers>,
    /// Maps to: CC `useIdeSelection(mcp.clients, setIDESelection)` — the
    /// REPL-owned selection sender. Stored here only as the threading path;
    /// the live per-connection sink is the clone captured by each "ide"
    /// entry in `CONNECTED_CLIENTS` (dropped with the entry — CC
    /// useIdeSelection.ts:148 "No cleanup needed").
    ide_selection_sink:
        Option<async_channel::Sender<crate::hooks::use_ide_selection::IdeSelection>>,
}

fn connection_attempt_handlers_slot() -> &'static StdMutex<ConnectionAttemptRegistry> {
    CONNECTION_ATTEMPT_HANDLERS.get_or_init(|| StdMutex::new(ConnectionAttemptRegistry::default()))
}

/// Maps to: CC `useIdeSelection` re-running its effect with the latest
/// `onSelect`: replaces the sender future "ide" connections will capture.
pub(crate) fn set_ide_selection_sink(
    sink: async_channel::Sender<crate::hooks::use_ide_selection::IdeSelection>,
) {
    connection_attempt_handlers_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .ide_selection_sink = Some(sink);
}

/// Sender a newly cached connection should capture (None until a REPL mounts).
pub(crate) fn current_ide_selection_sink()
-> Option<async_channel::Sender<crate::hooks::use_ide_selection::IdeSelection>> {
    connection_attempt_handlers_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .ide_selection_sink
        .clone()
}

/// Live writers captured like CC `onConnectionAttempt` closures
/// (`updateServer` / `setAppState`).
#[derive(Clone)]
pub struct OnConnectionAttemptHandlers {
    mcp: McpWriter,
    channel_permission_callbacks: Option<ChannelPermissionCallbacks>,
}

impl OnConnectionAttemptHandlers {
    pub fn new(
        mcp: McpWriter,
        channel_permission_callbacks: Option<ChannelPermissionCallbacks>,
    ) -> Self {
        Self {
            mcp,
            channel_permission_callbacks,
        }
    }

    /// Maps to: CC `client.client.onclose` body in `onConnectionAttempt`.
    pub async fn on_server_closed(&self, name: String, config: ScopedMcpServerConfig) {
        handle_mcp_server_closed_event(name, config, self.mcp.clone()).await;
    }

    /// Maps to: CC `ToolListChangedNotificationSchema` handler.
    pub async fn on_tools_list_changed(&self, name: String) {
        handle_mcp_tools_list_changed_event(name, self.mcp.clone()).await;
    }

    /// Maps to: CC `PromptListChangedNotificationSchema` handler.
    pub async fn on_prompts_list_changed(&self, name: String) {
        handle_mcp_prompts_list_changed_event(name, self.mcp.clone()).await;
    }

    /// Maps to: CC `ResourceListChangedNotificationSchema` handler.
    pub async fn on_resources_list_changed(&self, name: String) {
        handle_mcp_resources_list_changed_event(name, self.mcp.clone()).await;
    }

    /// Maps to: CC channel message notification handler in `onConnectionAttempt`.
    pub fn on_channel_message_received(
        &self,
        name: &str,
        content: &str,
        meta: Option<&std::collections::BTreeMap<String, String>>,
        has_channel_capability: bool,
        plugin_source: Option<&str>,
    ) {
        let gate = handle_channel_message_notification_event(
            name,
            content,
            meta,
            has_channel_capability,
            plugin_source,
        );
        if let ChannelGateResult::Skip { reason, .. } = gate {
            tracing::debug!(server = %name, reason = %reason, "MCP channel notification skipped");
        }
    }

    /// Maps to: CC channel permission notification handler in `onConnectionAttempt`.
    pub fn on_channel_permission_received(
        &self,
        name: &str,
        request_id: &str,
        behavior: crate::services::mcp::channel_permissions::ChannelPermissionBehavior,
        has_permission_capability: bool,
    ) {
        let Some(resolved) = handle_channel_permission_notification_event(
            self.channel_permission_callbacks.as_ref(),
            name,
            request_id,
            behavior,
            has_permission_capability,
        ) else {
            return;
        };
        tracing::debug!(
            server = %name,
            request_id = %request_id,
            resolved = resolved,
            "MCP channel permission notification received"
        );
    }

    /// Maps to: CC `registerElicitationHandler` → `setAppState` queue push.
    pub fn on_elicitation_requested(&self, event: ElicitationRequestEvent) {
        self.mcp.push_elicitation_event(event);
    }

    /// Maps to: CC elicitation-complete notification → `setAppState` queue update.
    pub fn on_elicitation_completed(&self, name: &str, elicitation_id: &str) {
        self.mcp.mark_elicitation_complete(name, elicitation_id);
    }
}

/// Maps to: CC mounting REPL/App with live `setAppState` / `updateServer` closures.
pub fn register_on_connection_attempt_handlers(hooks: OnConnectionAttemptHandlers) {
    connection_attempt_handlers_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .handlers = Some(hooks);
}

pub fn clear_on_connection_attempt_handlers() {
    connection_attempt_handlers_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .handlers = None;
}

fn with_connection_handlers<R>(f: impl FnOnce(&OnConnectionAttemptHandlers) -> R) -> Option<R> {
    let guard = connection_attempt_handlers_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.handlers.as_ref().map(f)
}

async fn with_connection_handlers_async<R, Fut>(
    f: impl FnOnce(OnConnectionAttemptHandlers) -> Fut,
) -> Option<R>
where
    Fut: std::future::Future<Output = R>,
{
    let hooks = {
        let guard = connection_attempt_handlers_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.handlers.clone()
    };
    if let Some(hooks) = hooks {
        Some(f(hooks).await)
    } else {
        None
    }
}

// --- emit helpers used by the rmcp client handler (CC setNotificationHandler bodies) ---

pub(crate) async fn emit_server_closed(name: String, config: ScopedMcpServerConfig) {
    let handled = with_connection_handlers_async({
        let name = name.clone();
        let config = config.clone();
        |hooks| async move { hooks.on_server_closed(name, config).await }
    })
    .await
    .is_some();
    if handled {
        return;
    }
    observe_for_test(McpConnectionCallbackObservation::ServerClosed { name, config }).await;
}

/// Maps to: CC `useManageMCPConnections.ts:618`
/// `if (client.capabilities?.tools?.listChanged)`.
///
/// At the source this gates handler REGISTRATION — a server that never declared
/// the capability simply has no handler installed. rmcp installs its handler
/// trait methods unconditionally, so the gate has to be evaluated per
/// notification instead. The predicate lives here rather than at the rmcp
/// handler in `client.rs` because the decision is this file's: `client.rs` is
/// the transport seam that happens to receive the notification, and
/// `useManageMCPConnections.ts` is what CC gives the choice to.
#[cfg(feature = "mcp_runtime")]
pub(crate) fn declares_tools_list_changed(capabilities: &rmcp::model::ServerCapabilities) -> bool {
    capabilities
        .tools
        .as_ref()
        .and_then(|capability| capability.list_changed)
        .unwrap_or(false)
}

/// Maps to: CC `useManageMCPConnections.ts:667`
/// `if (client.capabilities?.prompts?.listChanged)` — see
/// [`declares_tools_list_changed`] for why the gate moved inside.
#[cfg(feature = "mcp_runtime")]
pub(crate) fn declares_prompts_list_changed(
    capabilities: &rmcp::model::ServerCapabilities,
) -> bool {
    capabilities
        .prompts
        .as_ref()
        .and_then(|capability| capability.list_changed)
        .unwrap_or(false)
}

/// Maps to: CC `useManageMCPConnections.ts:705`
/// `if (client.capabilities?.resources?.listChanged)` — see
/// [`declares_tools_list_changed`] for why the gate moved inside.
///
/// Note this is the `listChanged` sub-capability, NOT `!!capabilities.resources`
/// (CC `client.ts:2169`), which is a separate question already answered by
/// `McpServerSnapshot::supports_resources`.
#[cfg(feature = "mcp_runtime")]
pub(crate) fn declares_resources_list_changed(
    capabilities: &rmcp::model::ServerCapabilities,
) -> bool {
    capabilities
        .resources
        .as_ref()
        .and_then(|capability| capability.list_changed)
        .unwrap_or(false)
}

pub(crate) async fn emit_tools_list_changed(name: String) {
    let handled = with_connection_handlers_async({
        let name = name.clone();
        |hooks| async move { hooks.on_tools_list_changed(name).await }
    })
    .await
    .is_some();
    if handled {
        return;
    }
    observe_for_test(McpConnectionCallbackObservation::ToolsListChanged { name }).await;
}

pub(crate) async fn emit_prompts_list_changed(name: String) {
    let handled = with_connection_handlers_async({
        let name = name.clone();
        |hooks| async move { hooks.on_prompts_list_changed(name).await }
    })
    .await
    .is_some();
    if handled {
        return;
    }
    observe_for_test(McpConnectionCallbackObservation::PromptsListChanged { name }).await;
}

pub(crate) async fn emit_resources_list_changed(name: String) {
    let handled = with_connection_handlers_async({
        let name = name.clone();
        |hooks| async move { hooks.on_resources_list_changed(name).await }
    })
    .await
    .is_some();
    if handled {
        return;
    }
    observe_for_test(McpConnectionCallbackObservation::ResourcesListChanged { name }).await;
}

pub(crate) async fn emit_channel_message_received(
    name: String,
    content: String,
    meta: Option<std::collections::BTreeMap<String, String>>,
    has_channel_capability: bool,
    plugin_source: Option<String>,
) {
    if with_connection_handlers(|hooks| {
        hooks.on_channel_message_received(
            &name,
            &content,
            meta.as_ref(),
            has_channel_capability,
            plugin_source.as_deref(),
        );
    })
    .is_some()
    {
        return;
    }
    observe_for_test(McpConnectionCallbackObservation::ChannelMessageReceived {
        name,
        content,
        meta,
        has_channel_capability,
        plugin_source,
    })
    .await;
}

pub(crate) async fn emit_channel_permission_received(
    name: String,
    request_id: String,
    behavior: crate::services::mcp::channel_permissions::ChannelPermissionBehavior,
    has_permission_capability: bool,
) {
    if with_connection_handlers(|hooks| {
        hooks.on_channel_permission_received(
            &name,
            &request_id,
            behavior,
            has_permission_capability,
        );
    })
    .is_some()
    {
        return;
    }
    observe_for_test(
        McpConnectionCallbackObservation::ChannelPermissionReceived {
            name,
            request_id,
            behavior,
            has_permission_capability,
        },
    )
    .await;
}

pub(crate) async fn emit_elicitation_requested(event: ElicitationRequestEvent) {
    if with_connection_handlers(|hooks| hooks.on_elicitation_requested(event.clone())).is_some() {
        return;
    }
    observe_for_test(McpConnectionCallbackObservation::ElicitationRequested { event }).await;
}

pub(crate) async fn emit_elicitation_completed(name: String, elicitation_id: String) {
    if with_connection_handlers(|hooks| hooks.on_elicitation_completed(&name, &elicitation_id))
        .is_some()
    {
        return;
    }
    observe_for_test(McpConnectionCallbackObservation::ElicitationCompleted {
        name,
        elicitation_id,
    })
    .await;
}

/// Observation queue used only when App has not registered session hooks
/// (unit tests that exercise the rmcp client without mounting App).
#[derive(Clone, Debug, PartialEq)]
pub enum McpConnectionCallbackObservation {
    ServerClosed {
        name: String,
        config: ScopedMcpServerConfig,
    },
    ToolsListChanged {
        name: String,
    },
    PromptsListChanged {
        name: String,
    },
    ResourcesListChanged {
        name: String,
    },
    ChannelMessageReceived {
        name: String,
        content: String,
        meta: Option<std::collections::BTreeMap<String, String>>,
        has_channel_capability: bool,
        plugin_source: Option<String>,
    },
    ChannelPermissionReceived {
        name: String,
        request_id: String,
        behavior: crate::services::mcp::channel_permissions::ChannelPermissionBehavior,
        has_permission_capability: bool,
    },
    ElicitationRequested {
        event: ElicitationRequestEvent,
    },
    ElicitationCompleted {
        name: String,
        elicitation_id: String,
    },
}

static TEST_OBSERVATIONS: OnceLock<AsyncMutex<VecDeque<McpConnectionCallbackObservation>>> =
    OnceLock::new();

fn test_observations() -> &'static AsyncMutex<VecDeque<McpConnectionCallbackObservation>> {
    TEST_OBSERVATIONS.get_or_init(|| AsyncMutex::new(VecDeque::new()))
}

async fn observe_for_test(observation: McpConnectionCallbackObservation) {
    test_observations().lock().await.push_back(observation);
}

/// Drain unhandled MCP session callback observations (tests without App hooks).
pub async fn drain_mcp_connection_callback_observations() -> Vec<McpConnectionCallbackObservation> {
    test_observations().lock().await.drain(..).collect()
}

// Native setTimeout(resolve) + Promise transport for the source backoff await.
// The registry contains only the timer; the connection Promise is detached.
fn schedule_mcp_reconnect_timer(
    name: &str,
    delay_ms: u64,
) -> futures::channel::oneshot::Receiver<()> {
    let (resolve, resume) = futures::channel::oneshot::channel();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(delay_ms);
    let handle = crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("MCP process runtime")
        .spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let _ = resolve.send(());
        });
    RECONNECT_TASKS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(name.to_owned(), ReconnectTaskEntry { handle });
    resume
}

/// Maps to: CC `reconnectWithBackoff` in
/// `services/mcp/useManageMCPConnections.ts#onConnectionAttempt`.
pub async fn start_automatic_mcp_reconnect(
    name: String,
    config: ScopedMcpServerConfig,
    runtime_mcp: McpWriter,
) {
    cancel_pending_mcp_reconnect(&name);
    // Source reconnectWithBackoff is an independent Promise. Only its
    // setTimeout(resolve) handle belongs to reconnectTimersRef, never an
    // in-flight connect Promise (cleanup must not abort that connection).
    crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("MCP process runtime")
        .spawn(async move {
            for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
                {
                    let _turn = crate::state::store::enter_store_turn_segment();
                    if is_mcp_server_disabled(&name) {
                        RECONNECT_TASKS
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&name);
                        return;
                    }
                    let base = reconnect_base_server(&name, &config, runtime_mcp.clone());
                    runtime_mcp
                        .apply_client_update(pending_reconnect_server_update(&base, attempt));
                }
                let result = reconnect_mcp_server_impl(&name, &config).await;
                let resume = {
                    let _turn = crate::state::store::enter_store_turn_segment();
                    if result.server.client.status == McpServerConnectionType::Connected
                        || attempt == MAX_RECONNECT_ATTEMPTS
                    {
                        RECONNECT_TASKS
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&name);
                        runtime_mcp.apply_server_update(result);
                        return;
                    }
                    schedule_mcp_reconnect_timer(&name, reconnect_backoff_ms(attempt))
                };
                // Aborted timer drops its resolver. Source remains suspended at
                // this await forever; native disposal releases the inert frame.
                if resume.await.is_err() {
                    return;
                }
            }
        });
}

/// Maps to: CC `useManageMCPConnections.ts#initializeServersAsPending`
/// stale-client cleanup guard: only connected clients have an active runtime
/// connection/cache to cancel after removing stale AppState entries.
pub fn stale_mcp_server_needs_cache_cleanup(server: &McpServerSnapshot) -> bool {
    server.client.status == McpServerConnectionType::Connected
}

/// Maps to: CC `useManageMCPConnections.ts#initializeServersAsPending` stale
/// cleanup loop. CC sets `client.onclose = undefined` before `clearServerCache`;
/// The writer detaches the captured client identity before invoking this helper;
/// cache lookup occurs synchronously and cancellation completes asynchronously.
pub fn cleanup_stale_mcp_clients(
    stale: &[McpServerSnapshot],
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let cleanups = stale
        .iter()
        .filter(|server| stale_mcp_server_needs_cache_cleanup(server))
        .map(|server| clear_server_cache(&server.client.name, server.config.as_ref()))
        .collect::<Vec<_>>();
    async move {
        futures::future::join_all(cleanups).await;
    }
}

/// Maps to: CC `useManageMCPConnections.ts#initializeServersAsPending`
/// stale-client reconciliation result.
#[derive(Clone, Debug, PartialEq)]
pub struct InitializeServersAsPendingResult {
    pub state: McpState,
    /// Stale clients are returned for service-boundary cleanup by the caller,
    /// matching CC's `clearServerCache` fire-and-forget loop.
    pub stale: Vec<McpServerSnapshot>,
    /// Names of the servers this call added. Maps to: CC `newClients`
    /// (`useManageMCPConnections.ts:816-825`). Its emptiness is half of the
    /// source's `return prevState` guard at `:826-828`.
    pub new_clients: Vec<String>,
}

/// Maps to: CC `useManageMCPConnections.ts#initializeServersAsPending`.
pub fn initialize_servers_as_pending(
    current_state: &McpState,
    configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
) -> InitializeServersAsPendingResult {
    let stale_result = super::utils::exclude_stale_plugin_clients(current_state, configs);
    // Source timer guards run synchronously inside the initialization updater.
    for server in &stale_result.stale {
        cancel_pending_mcp_reconnect(&server.client.name);
    }
    let mut state = stale_result.state;
    let existing_names = state
        .clients
        .iter()
        .map(|server| server.client.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut new_clients = Vec::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(configs)
        .into_iter()
        .filter(|(name, _)| !existing_names.contains(*name))
    {
        let server = if is_mcp_server_disabled(name) {
            McpConnectionDiscovery::disabled(name, config).server
        } else {
            McpConnectionDiscovery::pending_with_config(name, config).server
        };
        new_clients.push(name.to_owned());
        apply_mcp_server_update(&mut state, server);
    }
    InitializeServersAsPendingResult {
        state,
        stale: stale_result.stale,
        new_clients,
    }
}

/// Maps to: CC `useManageMCPConnections(...)` initial pending AppState update.
pub fn pending_mcp_state_from_configs(
    configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
) -> McpState {
    initialize_servers_as_pending(&McpState::default(), configs).state
}

/// Maps to: CC `useManageMCPConnections.ts:207#MCP_BATCH_FLUSH_MS`.
const MCP_BATCH_FLUSH_MS: u64 = 16;

/// Native retained carrier for the source pendingUpdatesRef/flushTimerRef and
/// updateServer/flushPendingUpdates callbacks. The existing McpWriter owns the
/// canonical single-store-update reducer; this carrier only owns the timer.
#[derive(Clone)]
struct McpPendingUpdates {
    writer: McpWriter,
    state: std::sync::Arc<std::sync::Mutex<McpPendingUpdatesState>>,
}

#[derive(Default)]
struct McpPendingUpdatesState {
    updates: Vec<McpPendingUpdate>,
    timer: Option<(u64, tokio::task::JoinHandle<()>)>,
    next_generation: u64,
}

impl McpPendingUpdates {
    fn new(writer: McpWriter) -> Self {
        Self {
            writer,
            state: Default::default(),
        }
    }

    /// Maps to: CC `useManageMCPConnections.ts:297#updateServer`.
    fn update_server(&self, server: impl Into<McpPendingUpdate>) {
        // One source synchronous callback cannot be interrupted by another
        // thread's timer. Reuse the store family's reentrant JS-turn carrier.
        let _turn = crate::state::store::enter_store_turn_segment();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.updates.push(server.into());
        if state.timer.is_none() {
            let pending = self.clone();
            let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("MCP update timers require the published process runtime");
            let deadline =
                tokio::time::Instant::now() + std::time::Duration::from_millis(MCP_BATCH_FLUSH_MS);
            state.next_generation += 1;
            let generation = state.next_generation;
            state.timer = Some((
                generation,
                runtime.spawn(async move {
                    tokio::time::sleep_until(deadline).await;
                    pending.flush_pending_updates(Some(generation));
                }),
            ));
        }
    }

    /// Maps to: CC `useManageMCPConnections.ts:216#flushPendingUpdates`.
    /// expected_timer only represents native callbacks that already woke and
    /// are waiting for the JS-turn gate when clearTimeout cancels their timer.
    fn flush_pending_updates(&self, expected_timer: Option<u64>) {
        let _turn = crate::state::store::enter_store_turn_segment();
        let updates = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(expected) = expected_timer {
                if state.timer.as_ref().map(|(generation, _)| *generation) != Some(expected) {
                    return;
                }
            }
            state.timer = None;
            std::mem::take(&mut state.updates)
        };
        // Release the non-reentrant data mutex before listeners run; the
        // reentrant turn remains held through the complete source store call.
        self.writer.apply_server_updates(updates);
    }

    /// Maps to: CC `useManageMCPConnections.ts:1035-1039` unmount cleanup.
    fn cleanup(&self) {
        let _turn = crate::state::store::enter_store_turn_segment();
        let timer = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .timer
            .take();
        if let Some((_, timer)) = timer {
            timer.abort();
            self.flush_pending_updates(None);
        }
    }
}

// The retained hook owns cleanup; callback clones may outlive an unmount just
// like already-started source promises, without keeping the hook mounted.
#[derive(Default)]
struct McpPendingUpdatesHook(Option<McpPendingUpdates>);
impl iocraft::prelude::Hook for McpPendingUpdatesHook {}
impl Drop for McpPendingUpdatesHook {
    fn drop(&mut self) {
        // Source third effect cleanup: synchronously clear all reconnect timers.
        for (_, task) in RECONNECT_TASKS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
        {
            task.handle.abort();
        }
        if let Some(pending) = &self.0 {
            pending.cleanup();
        }
    }
}

#[cfg(test)]
static MCP_DISCOVERY_STARTS: LazyLock<std::sync::Mutex<Vec<(String, Option<u64>)>>> =
    LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

// Native carrier for React effect dependencies. Keep the actual Arc/store
// alive: comparing a deep snapshot (or only a recycled pointer/hash) is not
// Object.is(dynamicMcpConfig/setAppState).
#[derive(Clone)]
struct McpDiscoveryDependencies {
    store: crate::state::store::AppStore,
    startup: std::sync::Arc<crate::main::McpStartupConfig>,
    session_id: String,
    plugin_reconnect_key: u64,
    auth_version: Option<u64>,
}
impl PartialEq for McpDiscoveryDependencies {
    fn eq(&self, other: &Self) -> bool {
        self.store.same_instance(&other.store)
            && std::sync::Arc::ptr_eq(&self.startup, &other.startup)
            && self.startup.strict == other.startup.strict
            && self.startup.bare == other.startup.bare
            && self.session_id == other.session_id
            && self.plugin_reconnect_key == other.plugin_reconnect_key
            && self.auth_version == other.auth_version
    }
}

// This only retains useEffect's launch/cleanup transport, not MCP policy. The
// initialization effect deliberately has no cancellation token; only the
// connection effect installs cleanup, and cleanup does not abort its Promise.
#[derive(Default)]
struct McpDiscoveryEffect {
    dependencies: Option<McpDiscoveryDependencies>,
    launch: Option<Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send>>,
    cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl McpDiscoveryEffect {
    fn update(
        &mut self,
        dependencies: Option<McpDiscoveryDependencies>,
        launch: impl FnOnce(
            Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        ) -> futures::future::BoxFuture<'static, ()>
        + Send
        + 'static,
        cancellable: bool,
    ) {
        let _turn = crate::state::store::enter_store_turn_segment();
        if self.dependencies == dependencies {
            return;
        }
        if let Some(cancelled) = self.cancelled.take() {
            cancelled.store(true, Ordering::SeqCst);
        }
        self.dependencies = dependencies;
        if self.dependencies.is_none() {
            self.launch = None;
            return;
        }
        let cancelled =
            cancellable.then(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        self.cancelled = cancelled.clone();
        self.launch = Some(Box::new(move || launch(cancelled)));
    }
}
// React starts effects in registration order and runs each async function up
// to its first suspension before starting the next. Poll those prefixes on the
// process runtime (filesystem work stays out of the iocraft update frame), then
// detach their independent continuations. Dependencies and cleanup stay separate.
#[derive(Default)]
struct McpDiscoveryEffects {
    initialize: McpDiscoveryEffect,
    connect: McpDiscoveryEffect,
}
impl iocraft::prelude::Hook for McpDiscoveryEffects {
    fn post_component_update(&mut self, _updater: &mut iocraft::prelude::ComponentUpdater) {
        let launches = [self.initialize.launch.take(), self.connect.launch.take()];
        if launches.iter().all(Option::is_none) {
            return;
        }
        let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work()
            .expect("MCP process runtime");
        let continuation_runtime = runtime.clone();
        runtime.spawn(async move {
            let mut continuations = Vec::new();
            for launch in launches.into_iter().flatten() {
                let mut future = launch();
                let completed = futures::future::poll_fn(|context| {
                    std::task::Poll::Ready(future.as_mut().poll(context).is_ready())
                })
                .await;
                if !completed {
                    continuations.push(future);
                }
            }
            // React completes the effect flush before Promise continuations.
            for future in continuations {
                continuation_runtime.spawn(future);
            }
        });
    }
}
impl Drop for McpDiscoveryEffect {
    fn drop(&mut self) {
        let _turn = crate::state::store::enter_store_turn_segment();
        if let Some(cancelled) = &self.cancelled {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
}

// Captures the existing native config getters off the render frame. This is
// the source's implicit getGlobalConfig/getProjectConfig environment, not a
// separate configuration-loading policy.
fn mcp_discovery_config_context() -> (
    crate::utils::config::GlobalConfig,
    crate::utils::config::ProjectConfig,
) {
    let global = crate::utils::config::load_global_config();
    let project = std::env::current_dir()
        .ok()
        .map(|cwd| crate::interactive_helpers::project_config_for_cwd(&global, &cwd))
        .unwrap_or_default();
    (global, project)
}

/// Maps to: CC useManageMCPConnections.ts:773 local initializeServersAsPending.
async fn initialize_discovered_servers_as_pending(
    dependencies: McpDiscoveryDependencies,
) -> anyhow::Result<()> {
    #[cfg(test)]
    MCP_DISCOVERY_STARTS
        .lock()
        .unwrap()
        .push((dependencies.session_id.clone(), None));
    let (global, project) = mcp_discovery_config_context();
    let loaded = if dependencies.startup.strict || dependencies.startup.bare {
        super::config::McpConfigs::default()
    } else {
        super::config::get_claude_code_mcp_configs(
            &global,
            &project,
            &dependencies.startup.dynamic,
            futures::future::ready(Default::default()),
        )
        .await?
    };
    let _turn = crate::state::store::enter_store_turn_segment();
    let mut configs = loaded.servers;
    configs.extend(dependencies.startup.dynamic.clone());
    add_errors_to_app_state(&dependencies.store, loaded.errors);
    McpWriter::new(dependencies.store).initialize_servers_as_pending(&configs);
    Ok(())
}

/// Maps to: CC useManageMCPConnections.ts:863 local loadAndConnectMcpConfigs.
async fn load_and_connect_mcp_configs(
    dependencies: McpDiscoveryDependencies,
    pending: McpPendingUpdates,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    #[cfg(test)]
    MCP_DISCOVERY_STARTS
        .lock()
        .unwrap()
        .push((dependencies.session_id.clone(), dependencies.auth_version));
    let (global, project) = mcp_discovery_config_context();
    let strict = dependencies.startup.strict || dependencies.startup.bare;
    // User-authorized L2 (2026-09-15): retain the source two-phase flow with an
    // empty remote result; claudeai.rs stays implemented but is not wired here.
    // In particular, do not clear its cache or consult Claude.ai login state.
    // Preserve only the shared MCP auth-failure cache invalidation previously
    // delegated by claudeai.ts#clearClaudeAIMcpConfigsCache, under its same guard.
    // This does not read Claude.ai credentials or invoke the retained module.
    if !strict && !super::config::does_enterprise_mcp_config_exist_readonly() {
        super::client::clear_mcp_auth_cache();
    }
    // Original wiring retained for future re-enablement:
    // let claudeai_promise = if strict || super::config::does_enterprise_mcp_config_exist_readonly() {
    //     super::config::start_mcp_config_promise(futures::future::ready(Default::default()))
    // } else {
    //     super::claudeai::clear_claude_ai_mcp_configs_cache();
    //     super::config::start_mcp_config_promise(
    //         super::claudeai::fetch_claude_ai_mcp_configs_if_eligible(),
    //     )
    // };
    let claudeai_promise =
        super::config::start_mcp_config_promise(futures::future::ready(Default::default()));
    let loaded = if strict {
        super::config::McpConfigs::default()
    } else {
        super::config::get_claude_code_mcp_configs(
            &global,
            &project,
            &dependencies.startup.dynamic,
            claudeai_promise.clone(),
        )
        .await?
    };
    let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("MCP process runtime");
    let mut configs = loaded.servers;
    configs.extend(dependencies.startup.dynamic.clone());
    {
        let _turn = crate::state::store::enter_store_turn_segment();
        if cancelled.load(Ordering::SeqCst) {
            return Ok(());
        }
        add_errors_to_app_state(&dependencies.store, loaded.errors);
        let enabled = crate::utils::process_env::ecmascript_object_entries(&configs)
            .into_iter()
            .filter(|(name, _)| !is_mcp_server_disabled(name))
            .map(|(name, config)| (name.to_owned(), config.clone()))
            .collect();
        let local_pending = pending.clone();
        runtime.spawn(async move {
            get_mcp_tools_commands_and_resources(
                |discovery| local_pending.update_server(discovery),
                &enabled,
            )
            .await;
        });
        if strict {
            log_discovered_mcp_server_counts(&configs, &Default::default());
            return Ok(());
        }
    }
    let allowed =
        super::config::filter_mcp_servers_by_policy_readonly(claudeai_promise.await).allowed;
    let _turn = crate::state::store::enter_store_turn_segment();
    if cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    let remote =
        super::config::dedup_claude_ai_mcp_servers_readonly(&allowed, &configs, &project).servers;
    let claudeai_configs = remote.clone();
    if !remote.is_empty() {
        // Source phase 2 only appends new clients; it must not perform the
        // phase 1 stale-plugin reconciliation against this remote subset.
        dependencies.store.set_state(|previous| {
            let clients: Vec<_> = crate::utils::process_env::ecmascript_object_entries(&remote)
                .into_iter()
                .filter(|(name, _)| {
                    !previous
                        .mcp
                        .clients
                        .iter()
                        .any(|client| client.client.name == *name)
                })
                .map(|(name, config)| {
                    if is_mcp_server_disabled(name) {
                        McpConnectionDiscovery::disabled(name, config).server
                    } else {
                        McpConnectionDiscovery::pending_with_config(name, config).server
                    }
                })
                .collect();
            if clients.is_empty() {
                return crate::state::store::UpdateDecision::Same(());
            }
            let mut next = (**previous).clone();
            std::sync::Arc::make_mut(&mut next.mcp)
                .clients
                .extend(clients);
            crate::state::store::UpdateDecision::Replace {
                next: std::sync::Arc::new(next),
                result: (),
            }
        });
        let enabled = crate::utils::process_env::ecmascript_object_entries(&remote)
            .into_iter()
            .filter(|(name, _)| !is_mcp_server_disabled(name))
            .map(|(name, config)| (name.to_owned(), config.clone()))
            .collect();
        runtime.spawn(async move {
            get_mcp_tools_commands_and_resources(
                |discovery| pending.update_server(discovery),
                &enabled,
            )
            .await;
        });
    }
    log_discovered_mcp_server_counts(&configs, &claudeai_configs);
    Ok(())
}

// Native extraction of source loadAndConnectMcpConfigs' final synchronous
// logEvent block, shared by the strict no-await and awaited phase-2 paths.
fn log_discovered_mcp_server_counts(
    configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    claudeai_configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
) {
    let mut all_configs = configs.clone();
    all_configs.extend(claudeai_configs.clone());
    let mut counts =
        serde_json::json!({"enterprise":0,"global":0,"project":0,"user":0,"plugin":0,"claudeai":0});
    let ant = crate::utils::process_env::var("USER_TYPE").as_deref() == Some("ant");
    let mut stdio_commands = Vec::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(&all_configs) {
        use super::types::ConfigScope;
        let scope = match config.scope {
            ConfigScope::Enterprise => Some("enterprise"),
            ConfigScope::User => Some("global"),
            ConfigScope::Project => Some("project"),
            ConfigScope::Local => Some("user"),
            ConfigScope::Dynamic => Some("plugin"),
            ConfigScope::ClaudeAi => Some("claudeai"),
            ConfigScope::Managed => None,
        };
        if let Some(scope) = scope {
            counts[scope] = serde_json::json!(counts[scope].as_u64().unwrap() + 1);
        }
        if ant && !is_mcp_server_disabled(name) && config.transport == Transport::Stdio {
            if let Some(command) = &config.command {
                stdio_commands.push(
                    command
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
        }
    }
    if !stdio_commands.is_empty() {
        stdio_commands.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        counts["stdio_commands"] = serde_json::json!(stdio_commands.join(","));
    }
    crate::services::analytics::log_event("tengu_mcp_servers", counts);
}

#[derive(Default)]
struct McpPendingStoreIdentity {
    store: Option<crate::state::store::AppStore>,
    pending: Option<McpPendingUpdates>,
}
impl iocraft::prelude::Hook for McpPendingStoreIdentity {}

/// Maps to: CC useManageMCPConnections.ts:772–1026 independent effects.
pub fn use_manage_mcp_connections(
    hooks: &mut iocraft::prelude::Hooks,
    app_store: Option<crate::state::store::AppStore>,
    mcp_startup: Option<std::sync::Arc<crate::main::McpStartupConfig>>,
) {
    // The source session getter is read every render. Include it in the store
    // projection too, so /clear's state notification schedules that render
    // even when auth/plugin counters themselves have not changed.
    let observed =
        crate::state::app_state::use_app_state_maybe_outside_of_provider(hooks, |state| {
            (
                state.auth_version,
                state.mcp.plugin_reconnect_key,
                crate::bootstrap::state::get_session_id(),
            )
        });
    let (auth_version, plugin_reconnect_key, session_id) = observed.unwrap_or_else(|| {
        app_store
            .as_ref()
            .map(|store| {
                let state = store.get();
                (
                    state.auth_version,
                    state.mcp.plugin_reconnect_key,
                    crate::bootstrap::state::get_session_id(),
                )
            })
            .unwrap_or_default()
    });
    let dependencies =
        app_store
            .clone()
            .zip(mcp_startup)
            .map(|(store, startup)| McpDiscoveryDependencies {
                store,
                startup,
                session_id,
                plugin_reconnect_key,
                auth_version: None,
            });
    let identity = hooks.use_hook(McpPendingStoreIdentity::default);
    let changed_store = match (&identity.store, &app_store) {
        (Some(old), Some(new)) => !old.same_instance(new),
        (None, None) => false,
        _ => true,
    };
    identity.store = app_store.clone();
    let old_pending = if changed_store {
        identity.pending.take()
    } else {
        None
    };
    if changed_store {
        identity.pending = app_store.map(|store| McpPendingUpdates::new(McpWriter::new(store)));
    }
    let pending = identity.pending.clone();
    let cleanup_pending = pending.clone();
    use futures::FutureExt;
    let effects = hooks.use_hook(McpDiscoveryEffects::default);
    let init_dependencies = dependencies.clone();
    effects.initialize.update(
        dependencies.clone(),
        move |_| {
            async move {
                let Some(dependencies) = init_dependencies else {
                    return;
                };
                if let Err(error) = initialize_discovered_servers_as_pending(dependencies).await {
                    crate::utils::log::log_mcp_error(
                        "useManageMCPConnections",
                        serde_json::Value::String(format!(
                            "Failed to initialize servers as pending: {error}"
                        )),
                    );
                }
            }
            .boxed()
        },
        false,
    );
    let dependencies = dependencies.map(|mut deps| {
        deps.auth_version = Some(auth_version);
        deps
    });
    let connect_dependencies = dependencies.clone();
    effects.connect.update(
        dependencies,
        move |cancelled| {
            async move {
                let Some((dependencies, pending)) = connect_dependencies.zip(pending) else {
                    return;
                };
                if let Err(error) = load_and_connect_mcp_configs(
                    dependencies,
                    pending,
                    cancelled.expect("connection cleanup token"),
                )
                .await
                {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                        error.to_string(),
                    ));
                }
            }
            .boxed()
        },
        true,
    );
    // Register source cleanup effect third: connection cancellation is set
    // before timer cleanup/flush during unmount, matching React effect order.
    if let Some(old) = old_pending {
        old.cleanup();
    }
    hooks.use_hook(McpPendingUpdatesHook::default).0 = cleanup_pending;
}

/// Maps to: CC `useMcpReconnect()` callback backed by
/// `reconnectMcpServerImpl(...)`.
pub async fn reconnect_mcp_server_once(
    name: &str,
    config: &ScopedMcpServerConfig,
) -> McpConnectionDiscovery {
    cancel_pending_mcp_reconnect(name);
    reconnect_mcp_server_impl(name, config).await
}

/// Maps to: CC `useManageMCPConnections.ts:208#PendingUpdate`.
/// Optional dimensions preserve the source distinction between absent and empty.
#[derive(Clone, Debug)]
pub struct McpPendingUpdate {
    pub server: McpServerSnapshot,
    pub tools: Option<Vec<crate::types::tools::Tool>>,
    pub commands: Option<Vec<crate::commands::Command>>,
    pub resources: Option<Vec<super::types::ServerResource>>,
}

impl From<McpConnectionDiscovery> for McpPendingUpdate {
    fn from(update: McpConnectionDiscovery) -> Self {
        Self {
            server: update.server,
            tools: Some(update.tools),
            commands: Some(update.commands),
            resources: update.resources,
        }
    }
}

// Native full-snapshot callers provide all dimensions; callback callers retain
// their actual supplied tools (including the source-positioned resource helpers).
impl From<McpServerSnapshot> for McpPendingUpdate {
    fn from(server: McpServerSnapshot) -> Self {
        Self {
            tools: Some(super::client::mcp_tools_for_server_snapshot(&server)),
            commands: Some(super::client::mcp_commands_for_server_snapshot(&server)),
            resources: Some(server.resources.clone()),
            server,
        }
    }
}

/// Maps to: CC `useManageMCPConnections.ts#flushPendingUpdates`.
pub fn apply_mcp_server_update(state: &mut McpState, update: impl Into<McpPendingUpdate>) {
    let McpPendingUpdate {
        mut server,
        mut tools,
        mut commands,
        mut resources,
    } = update.into();
    let name = server.client.name.clone();
    if matches!(
        server.client.status,
        McpServerConnectionType::Disabled | McpServerConnectionType::Failed
    ) {
        tools.get_or_insert_with(Vec::new);
        commands.get_or_insert_with(Vec::new);
        resources.get_or_insert_with(Vec::new);
    }
    // Snapshots retain native remote capability caches. Undefined callback
    // dimensions preserve existing caches; for a new client they contribute
    // no cache. A forced empty dimension also clears its native cache.
    let previous = state
        .clients
        .iter()
        .find(|client| client.client.name == name);
    if tools.is_none() {
        server.tools = previous.map(|p| p.tools.clone()).unwrap_or_default();
    } else if tools.as_ref().is_some_and(Vec::is_empty) {
        server.tools.clear();
    }
    if commands.is_none() {
        server.prompts = previous.map(|p| p.prompts.clone()).unwrap_or_default();
    } else if commands.as_ref().is_some_and(Vec::is_empty) {
        server.prompts.clear();
    }
    if resources.is_none() {
        server.resources = previous.map(|p| p.resources.clone()).unwrap_or_default();
    } else {
        server.resources = resources.as_ref().unwrap().clone();
    }
    if let Some(tools) = tools {
        let prefix = format!(
            "mcp__{}__",
            super::normalization::normalize_name_for_mcp(&name)
        );
        state.tools.retain(|tool| !tool.name.starts_with(&prefix));
        state.tools.extend(tools);
    }
    if let Some(commands) = commands {
        state.commands.retain(|command| {
            !super::utils::command_belongs_to_server(command.name.as_ref(), &name)
        });
        state.commands.extend(commands);
    }
    if let Some(resources) = resources {
        // The source spreads the old map BEFORE spreading omit(old, name).
        // Consequently [] retains an existing key; it does not delete it.
        if !resources.is_empty() {
            state.resources.insert(name.clone(), resources);
        }
    }
    if let Some(previous) = state
        .clients
        .iter_mut()
        .find(|client| client.client.name == name)
    {
        *previous = server;
    } else {
        state.clients.push(server);
    }
}

/// Maps to: CC `updateServer({...client})`, with absent capability dimensions.
pub fn apply_mcp_client_update(state: &mut McpState, server: McpServerSnapshot) {
    apply_mcp_server_update(
        state,
        McpPendingUpdate {
            server,
            tools: None,
            commands: None,
            resources: None,
        },
    );
}

/// Maps to: CC `useManageMCPConnections.ts#tools/list_changed` updateServer.
pub fn apply_mcp_tools_list_changed(state: &mut McpState, server: McpServerSnapshot) {
    let tools = Some(super::client::mcp_tools_for_server_snapshot(&server));
    apply_mcp_server_update(
        state,
        McpPendingUpdate {
            server,
            tools,
            commands: None,
            resources: None,
        },
    );
}

/// Maps to: CC `useManageMCPConnections.ts#prompts/list_changed` updateServer.
pub fn apply_mcp_prompts_list_changed(state: &mut McpState, server: McpServerSnapshot) {
    let commands = Some(super::client::mcp_commands_for_server_snapshot(&server));
    apply_mcp_server_update(
        state,
        McpPendingUpdate {
            server,
            tools: None,
            commands,
            resources: None,
        },
    );
}

/// Maps to: CC `useManageMCPConnections.ts#resources/list_changed` updateServer.
pub fn apply_mcp_resources_list_changed(state: &mut McpState, server: McpServerSnapshot) {
    let resources = Some(server.resources.clone());
    apply_mcp_server_update(
        state,
        McpPendingUpdate {
            server,
            tools: None,
            commands: None,
            resources,
        },
    );
}

/// Maps to: CC `useMcpToggleEnabled()` / `toggleMcpServer`.
pub async fn toggle_mcp_server_once(
    name: &str,
    current_state: &McpState,
    config: &ScopedMcpServerConfig,
) -> anyhow::Result<McpConnectionDiscovery> {
    cancel_pending_mcp_reconnect(name);
    let client = current_state
        .clients
        .iter()
        .find(|server| server.client.name == name)
        .map(|server| &server.client)
        .ok_or_else(|| anyhow::anyhow!("MCP server {name} not found"))?;
    let is_currently_disabled = client.status == McpServerConnectionType::Disabled;

    if !is_currently_disabled {
        set_mcp_server_enabled(name, false)?;
        if client.status == McpServerConnectionType::Connected {
            clear_server_cache(name, None).await;
        }
        return Ok(McpConnectionDiscovery::disabled(name, config));
    }

    set_mcp_server_enabled(name, true)?;
    Ok(reconnect_mcp_server_impl(name, config).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mcp::types::{ConfigScope, ScopedMcpServerConfig, Transport};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn mounted_mcp_discovery_effects_follow_identity_auth_and_clear_session_dependencies() {
        use crate::state::app_state::{AppStateProvider, ProviderChildren};
        use crate::state::app_state_store::AppState;
        use crate::state::store::AppStore;
        use futures::{StreamExt, stream};
        use iocraft::prelude::*;
        use std::sync::{Arc, Mutex as StdMutex};
        let _env = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_runtime::initialize_test_process_runtime();
        struct SessionRestore(String);
        impl Drop for SessionRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_session_id(self.0.clone());
            }
        }
        let _session = SessionRestore(crate::bootstrap::state::get_session_id());
        let first_session = format!("mcp-effect-{}", uuid::Uuid::new_v4());
        let second_session = format!("{first_session}-clear");
        crate::bootstrap::state::set_session_id(first_session.clone());
        MCP_DISCOVERY_STARTS.lock().unwrap().clear();
        #[derive(Default, Props)]
        struct FixtureProps {
            store: Option<AppStore>,
            startup: Option<Arc<StdMutex<Arc<crate::main::McpStartupConfig>>>>,
        }
        #[component]
        fn Fixture(props: &FixtureProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let verbose = crate::state::app_state::use_app_state(&mut hooks, |state| state.verbose);
            let startup = props.startup.as_ref().unwrap().lock().unwrap().clone();
            element! { super::super::mcp_connection_manager::McpConnectionManager(app_store: props.store.clone(), mcp_startup: Some(startup)) { Text(content: format!("frame:{verbose}")) } }
        }
        let store = AppStore::new(AppState::default(), None);
        let startup = Arc::new(StdMutex::new(Arc::new(crate::main::McpStartupConfig {
            strict: true,
            ..Default::default()
        })));
        let tree_store = store.clone();
        let tree_startup = startup.clone();
        let mut element = element! { AppStateProvider(prebuilt_store: Some(store.clone()), children: ProviderChildren::new(move || element! { Fixture(store: Some(tree_store.clone()), startup: Some(tree_startup.clone())) }.into_any())) };
        futures::executor::block_on(async {
            let mut render = Box::pin(
                element.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::pending::<TerminalEvent>())
                        .with_size(30, 3),
                ),
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            let mut step = 0;
            let mut last_canvas = String::new();
            loop {
                if let Some(canvas) = crate::utils::race(render.next(), async {
                    futures_timer::Delay::new(std::time::Duration::from_millis(10)).await;
                    None
                })
                .await
                {
                    last_canvas = canvas.to_string();
                }
                let starts = MCP_DISCOVERY_STARTS.lock().unwrap().clone();
                match step {
                    0 if starts.len() == 2 => {
                        assert_eq!(
                            starts[0].1, None,
                            "strict initialization starts before connection"
                        );
                        assert_eq!(starts[1].1, Some(0));
                        assert_eq!(starts.iter().filter(|(_, auth)| auth.is_none()).count(), 1);
                        store.replace_with(|state| state.auth_version += 1);
                        step = 1;
                    }
                    1 if starts.len() == 3 => {
                        assert_eq!(
                            starts.iter().filter(|(_, auth)| auth.is_none()).count(),
                            1,
                            "auth-only must not reseed pending"
                        );
                        // Same Arc, new ordinary render: neither source dependency changes.
                        store.replace_with(|state| state.verbose = !state.verbose);
                        step = 2;
                    }
                    2 if last_canvas.contains("frame:true") => {
                        assert_eq!(starts.len(), 3);
                        let same_value_new_identity = Arc::new((**startup.lock().unwrap()).clone());
                        *startup.lock().unwrap() = same_value_new_identity;
                        store.replace_with(|state| state.verbose = !state.verbose);
                        step = 3;
                    }
                    3 if starts.len() == 5 => {
                        assert_eq!(starts.iter().filter(|(_, auth)| auth.is_none()).count(), 2);
                        // /clear switches the canonical session id and clears MCP state;
                        // auth and plugin counters intentionally stay unchanged.
                        crate::bootstrap::state::set_session_id(second_session.clone());
                        store.replace_with(|state| {
                            let key = state.mcp.plugin_reconnect_key;
                            state.mcp = Arc::new(McpState {
                                plugin_reconnect_key: key,
                                ..Default::default()
                            });
                        });
                        step = 4;
                    }
                    4 if starts.len() == 7 => {
                        assert_eq!(
                            starts
                                .iter()
                                .filter(|(session, _)| session == &second_session)
                                .count(),
                            2
                        );
                        assert_eq!(store.get().auth_version, 1);
                        assert_eq!(store.get().mcp.plugin_reconnect_key, 0);
                        break;
                    }
                    _ => {}
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "step={step}, starts={starts:?}, canvas={last_canvas:?}"
                );
            }
        });
    }

    #[test]
    fn discovery_effect_cleanup_matches_source_independent_cancel_and_identity_oracle() {
        use std::sync::Arc;
        let oracle: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/oracles/mcp-contract-review-0915/oracle.json")).unwrap();
        let base = McpDiscoveryDependencies {
            store: crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            ),
            startup: Arc::new(crate::main::McpStartupConfig::default()),
            session_id: "fixture".into(),
            plugin_reconnect_key: 0,
            auth_version: None,
        };
        let mut initial = McpDiscoveryEffect::default();
        initial.update(Some(base.clone()), |_| Box::pin(async {}), false);
        assert!(initial.cancelled.is_none());
        let mut connection = McpDiscoveryEffect::default();
        let mut connection_deps = base.clone();
        connection_deps.auth_version = Some(0);
        connection.update(Some(connection_deps.clone()), |_| Box::pin(async {}), true);
        let old = connection.cancelled.clone().unwrap();
        connection_deps.auth_version = Some(1);
        connection.update(Some(connection_deps), |_| Box::pin(async {}), true);
        assert!(old.load(Ordering::SeqCst));
        let current = connection.cancelled.clone().unwrap();
        assert!(!current.load(Ordering::SeqCst));
        drop(connection);
        assert!(current.load(Ordering::SeqCst));
        let mut new_identity = base.clone();
        new_identity.startup = Arc::new((*base.startup).clone());
        assert_eq!(
            base == base.clone(),
            oracle["dependencyCases"]["sameDynamicInitialEqual"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            base == new_identity,
            oracle["dependencyCases"]["equalNewDynamicInitialEqual"]
                .as_bool()
                .unwrap()
        );
        let mut new_store = base.clone();
        new_store.store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        assert!(
            base != new_store,
            "setAppState identity is a real source dependency"
        );
    }

    #[test]
    fn optional_resource_updates_match_actual_bun_flush_oracle() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/oracles/mcp-contract-review-0915/oracle.json")).unwrap();
        let resource = |server: &str, uri: &str| super::super::types::ServerResource {
            server: server.into(),
            uri: uri.into(),
            name: uri.into(),
            description: None,
            mime_type: None,
        };
        for case in oracle["resourceCases"].as_array().unwrap() {
            let label = case["label"].as_str().unwrap();
            let mut state = McpState::default();
            state
                .resources
                .insert("a".into(), vec![resource("a", "old")]);
            state
                .resources
                .insert("b".into(), vec![resource("b", "other")]);
            let mut server = McpConnectionDiscovery::pending("a").server;
            if label == "failed" {
                server.client.status = McpServerConnectionType::Failed;
            }
            let resources = match label {
                "empty" => Some(vec![]),
                "nonempty" => Some(vec![resource("a", "new")]),
                _ => None,
            };
            apply_mcp_server_update(
                &mut state,
                McpPendingUpdate {
                    server,
                    tools: None,
                    commands: None,
                    resources,
                },
            );
            let observed: serde_json::Map<String, serde_json::Value> = state
                .resources
                .iter()
                .map(|(name, values)| {
                    (
                        name.clone(),
                        serde_json::json!(
                            values
                                .iter()
                                .map(|r| serde_json::json!({"server": r.server, "uri": r.uri}))
                                .collect::<Vec<_>>()
                        ),
                    )
                })
                .collect();
            assert_eq!(
                serde_json::Value::Object(observed),
                case["resources"],
                "{label}"
            );
        }
    }

    #[test]
    fn callback_tool_order_is_retained_without_global_helper_reconstruction() {
        let mut state = McpState::default();
        let helper = crate::tools::list_mcp_resources_tool::list_mcp_resources_tool_schema();
        let mut first = McpConnectionDiscovery::pending("a");
        first.server.client.status = McpServerConnectionType::Connected;
        first.tools = vec![helper.clone()];
        apply_mcp_server_update(&mut state, first);
        let mut second = McpConnectionDiscovery::pending("b");
        second.server.client.status = McpServerConnectionType::Connected;
        let mut remote = helper.clone();
        remote.name = "mcp__b__remote".into();
        second.tools = vec![remote];
        apply_mcp_server_update(&mut state, second);
        assert_eq!(
            state
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            [helper.name.as_str(), "mcp__b__remote"]
        );
        let mut repeated = McpConnectionDiscovery::pending("c");
        repeated.tools = vec![helper.clone()];
        apply_mcp_server_update(&mut state, repeated);
        assert_eq!(
            state.tools.iter().filter(|t| t.name == helper.name).count(),
            2,
            "interactive source does not uniqBy native helpers across invocations"
        );
        apply_mcp_client_update(&mut state, McpConnectionDiscovery::pending("b").server);
        assert_eq!(
            state.tools.len(),
            3,
            "client-only pending preserves callback dimensions"
        );
    }

    #[tokio::test]
    async fn connection_completions_match_official_batch_window_and_unmount_flush() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let mut app = crate::state::app_state_store::AppState::default();
        app.mcp = std::sync::Arc::new(McpState {
            clients: vec![
                McpConnectionDiscovery::pending("slow").server,
                McpConnectionDiscovery::pending("fast").server,
            ],
            ..Default::default()
        });
        let store = crate::state::store::AppStore::new(app, None);
        let writer = McpWriter::new(store.clone());
        let pending = McpPendingUpdates::new(writer.clone());
        let owner = McpPendingUpdatesHook(Some(pending.clone()));
        let (changed_tx, changed_rx) = async_channel::unbounded();
        let observer = writer.clone();
        store.subscribe(std::sync::Arc::new(move || {
            let _ = changed_tx.try_send(observer.current());
        }));
        let (slow_tx, slow_rx) = tokio::sync::oneshot::channel();
        let (fast_tx, fast_rx) = tokio::sync::oneshot::channel();
        let (started_tx, started_rx) = async_channel::unbounded();
        let processing = super::super::client::process_batched(
            vec![("slow", slow_rx), ("fast", fast_rx)],
            2,
            |(name, gate)| {
                let pending = pending.clone();
                let started_tx = started_tx.clone();
                async move {
                    started_tx.send(name).await.unwrap();
                    gate.await.unwrap();
                    pending.update_server(
                        McpConnectionDiscovery::failed(name, &stdio_config(name), "controlled")
                            .server,
                    );
                }
            },
        );
        let observe = async {
            let mut started = vec![
                started_rx.recv().await.unwrap(),
                started_rx.recv().await.unwrap(),
            ];
            started.sort();
            assert_eq!(started, ["fast", "slow"]);
            let released_at = std::time::Instant::now();
            fast_tx.send(()).unwrap();
            let first = changed_rx.recv().await.unwrap();
            assert!(released_at.elapsed() >= std::time::Duration::from_millis(MCP_BATCH_FLUSH_MS));
            assert_eq!(
                first.clients[0].client.status,
                McpServerConnectionType::Pending
            );
            assert_eq!(
                first.clients[1].client.status,
                McpServerConnectionType::Failed
            );
            assert_eq!(
                store.revision(),
                1,
                "fast completion is visible before slow completes"
            );
            slow_tx.send(()).unwrap();
        };
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(processing, observe);
        })
        .await
        .expect("a blocked server must not hold back a completed server's flush");
        // Source cleanup cancels the pending timer and synchronously flushes.
        drop(owner);
        assert_eq!(
            writer.current().clients[0].client.status,
            McpServerConnectionType::Failed
        );
        assert_eq!(store.revision(), 2);
        pending.cleanup();
        assert_eq!(
            store.revision(),
            2,
            "cleanup does not flush a completed batch twice"
        );
    }

    #[tokio::test]
    async fn synchronous_connection_updates_match_official_single_flush() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let writer = McpWriter::new(store.clone());
        let pending = McpPendingUpdates::new(writer.clone());
        // The source accumulates same-turn callbacks into one timer window.
        // Execute on the same process loop as the timer to retain JS ordering.
        let (completed, observed) = tokio::sync::oneshot::channel();
        let process_pending = pending.clone();
        crate::utils::process_runtime::process_runtime_handle()
            .unwrap()
            .spawn(async move {
                process_pending.update_server(McpConnectionDiscovery::pending("z").server);
                process_pending.update_server(McpConnectionDiscovery::pending("a").server);
                completed.send(()).unwrap();
            });
        observed.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while store.revision() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.revision(), 1);
        assert_eq!(
            writer
                .current()
                .clients
                .iter()
                .map(|server| server.client.name.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        pending.cleanup();
    }

    #[test]
    fn mcp_flush_matches_official_indivisible_store_turn_across_threads() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let pending = McpPendingUpdates::new(McpWriter::new(store.clone()));
        let turn = crate::state::store::enter_store_turn_segment();
        pending.update_server(McpConnectionDiscovery::pending("old").server);
        let generation = pending.state.lock().unwrap().timer.as_ref().unwrap().0;
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let other = pending.clone();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            other.flush_pending_updates(Some(generation));
            finished_tx.send(()).unwrap();
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_millis(30))
                .is_err()
        );
        // The old implementation drained this queue before blocking inside
        // McpWriter, allowing a newer flush to overtake the detached old batch.
        assert_eq!(pending.state.lock().unwrap().updates.len(), 1);
        assert!(pending.state.lock().unwrap().timer.is_some());
        pending.update_server(McpConnectionDiscovery::pending("new").server);
        drop(turn);
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        worker.join().unwrap();
        assert_eq!(store.revision(), 1);
        assert_eq!(
            pending
                .writer
                .current()
                .clients
                .iter()
                .map(|server| server.client.name.as_str())
                .collect::<Vec<_>>(),
            ["old", "new"]
        );
        pending.cleanup();
    }

    #[test]
    fn mcp_cleanup_matches_official_reentry_and_canceled_timer_generation() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let pending = McpPendingUpdates::new(McpWriter::new(store.clone()));
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_pending = pending.clone();
        store.subscribe(std::sync::Arc::new(move || {
            if !fired.swap(true, Ordering::SeqCst) {
                // Real synchronous store listener reentry must not deadlock
                // on the pending-data mutex or lose the newly armed timer.
                callback_pending.update_server(McpConnectionDiscovery::pending("reentrant").server);
            }
        }));
        let (finished, observed) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _turn = crate::state::store::enter_store_turn_segment();
            pending.update_server(McpConnectionDiscovery::pending("first").server);
            let canceled_generation = pending.state.lock().unwrap().timer.as_ref().unwrap().0;
            pending.cleanup();
            assert_eq!(store.revision(), 1);
            let next_generation = pending.state.lock().unwrap().timer.as_ref().unwrap().0;
            assert_ne!(next_generation, canceled_generation);
            // An old callback may already be awake and waiting for the turn
            // when abort is called. It must never consume this new batch.
            pending.flush_pending_updates(Some(canceled_generation));
            assert_eq!(store.revision(), 1);
            assert_eq!(pending.state.lock().unwrap().updates.len(), 1);
            assert_eq!(
                pending.state.lock().unwrap().timer.as_ref().unwrap().0,
                next_generation
            );
            pending.cleanup();
            assert_eq!(store.revision(), 2);
            assert!(pending.state.lock().unwrap().timer.is_none());
            assert_eq!(
                pending
                    .writer
                    .current()
                    .clients
                    .iter()
                    .map(|server| server.client.name.as_str())
                    .collect::<Vec<_>>(),
                ["first", "reentrant"]
            );
            finished.send(()).unwrap();
        });
        observed
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("flush listener must be able to reenter updateServer");
        worker.join().unwrap();
    }

    #[test]
    fn mcp_batch_deadline_matches_official_set_timeout_registration_time() {
        // Advance a paused clock before the spawned timer's first poll. This
        // models a busy source event loop without a flaky wall-clock deadline.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        crate::utils::process_runtime::set_process_runtime_handle(runtime.handle().clone());
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let pending = McpPendingUpdates::new(McpWriter::new(store.clone()));
        let (changed_tx, changed_rx) = async_channel::unbounded();
        store.subscribe(std::sync::Arc::new(move || {
            let _ = changed_tx.try_send(());
        }));
        runtime.block_on(async {
            // Register inside the runtime so Instant::now uses its paused clock.
            pending.update_server(McpConnectionDiscovery::pending("delayed-poll").server);
            tokio::time::advance(std::time::Duration::from_millis(MCP_BATCH_FLUSH_MS + 10)).await;
            tokio::time::timeout(std::time::Duration::from_millis(8), changed_rx.recv())
                .await
                .expect("an already-expired timer must not start another 16ms window on first poll")
                .unwrap();
        });
        assert_eq!(store.revision(), 1);
        pending.cleanup();
    }

    #[test]
    fn plugin_errors_match_official_previous_state_only_dedup() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let error = crate::types::plugin::PluginError::GenericError {
            source: "same".into(),
            plugin: Some("plugin".into()),
            error: "first".into(),
        };
        add_errors_to_app_state(&store, vec![error.clone(), error.clone()]);
        assert_eq!(store.get().plugins.errors.len(), 2);
        let revision = store.revision();
        add_errors_to_app_state(&store, vec![error]);
        assert_eq!(store.revision(), revision);
    }

    /// Maps to: CC `useManageMCPConnections.ts:618/:667/:705` — the guard is
    /// `client.capabilities?.X?.listChanged`, so BOTH an absent capability and
    /// a declared capability without the sub-flag are falsy, and only an
    /// explicit `true` registers the handler.
    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn list_changed_gates_match_official_optional_chaining() {
        use rmcp::model::{
            PromptsCapability, ResourcesCapability, ServerCapabilities, ToolsCapability,
        };

        // `Option<Option<bool>>` mirrors what the guard can meet:
        // `None` = capability absent, `Some(None)` = declared without the
        // sub-flag, `Some(Some(v))` = declared explicitly. rmcp's capability
        // structs are `#[non_exhaustive]`, so they are built by mutation.
        fn caps(
            tools: Option<Option<bool>>,
            prompts: Option<Option<bool>>,
            resources: Option<Option<bool>>,
        ) -> ServerCapabilities {
            let mut capabilities = ServerCapabilities::default();
            if let Some(list_changed) = tools {
                let mut capability = ToolsCapability::default();
                capability.list_changed = list_changed;
                capabilities.tools = Some(capability);
            }
            if let Some(list_changed) = prompts {
                let mut capability = PromptsCapability::default();
                capability.list_changed = list_changed;
                capabilities.prompts = Some(capability);
            }
            if let Some(list_changed) = resources {
                let mut capability = ResourcesCapability::default();
                capability.list_changed = list_changed;
                capabilities.resources = Some(capability);
            }
            capabilities
        }

        // Capability absent entirely -> `capabilities?.tools` is undefined.
        let absent = caps(None, None, None);
        assert!(!declares_tools_list_changed(&absent));
        assert!(!declares_prompts_list_changed(&absent));
        assert!(!declares_resources_list_changed(&absent));

        // Declared but the sub-flag omitted -> `?.listChanged` is undefined,
        // which CC treats as falsy.
        let silent = caps(Some(None), Some(None), Some(None));
        assert!(!declares_tools_list_changed(&silent));
        assert!(!declares_prompts_list_changed(&silent));
        assert!(!declares_resources_list_changed(&silent));

        // Explicit false is still false.
        let denied = caps(Some(Some(false)), None, None);
        assert!(!declares_tools_list_changed(&denied));

        // Only an explicit true opens the gate, and each capability gates only
        // its own notification.
        let tools_only = caps(Some(Some(true)), None, None);
        assert!(declares_tools_list_changed(&tools_only));
        assert!(!declares_prompts_list_changed(&tools_only));
        assert!(!declares_resources_list_changed(&tools_only));

        let all = caps(Some(Some(true)), Some(Some(true)), Some(Some(true)));
        assert!(declares_tools_list_changed(&all));
        assert!(declares_prompts_list_changed(&all));
        assert!(declares_resources_list_changed(&all));
    }

    fn stdio_config(command: &str) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            name: None,
            scope: ConfigScope::User,
            transport: Transport::Stdio,
            command: Some(command.to_string()),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            url: None,
            headers: std::collections::BTreeMap::new(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: None,
            plugin_source: None,
        }
    }

    fn claude_ai_proxy_config(id: Option<&str>) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            name: None,
            scope: ConfigScope::ClaudeAi,
            transport: Transport::ClaudeAiProxy,
            command: None,
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            url: Some("https://mcp-proxy.anthropic.com/v1/mcp/mcpsrv_1".to_string()),
            headers: std::collections::BTreeMap::new(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: id.map(ToOwned::to_owned),
            plugin_source: None,
        }
    }

    fn with_isolated_config(test: impl FnOnce(&std::path::Path)) {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("cometix-mcp-manage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &temp_dir);
        test(&temp_dir);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    struct ProjectCwdGuard {
        previous_cwd: std::path::PathBuf,
        previous_original_cwd: std::path::PathBuf,
    }

    impl ProjectCwdGuard {
        fn enter(project_dir: &std::path::Path) -> Self {
            let previous_cwd = std::env::current_dir().unwrap();
            let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
            std::env::set_current_dir(project_dir).unwrap();
            let current_project_dir = std::env::current_dir().unwrap();
            crate::bootstrap::state::set_original_cwd(current_project_dir);
            Self {
                previous_cwd,
                previous_original_cwd,
            }
        }
    }

    impl Drop for ProjectCwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous_cwd);
            crate::bootstrap::state::set_original_cwd(self.previous_original_cwd.clone());
        }
    }

    #[test]
    fn channel_permission_notification_event_resolves_session_callbacks_and_skips_stale() {
        let callbacks =
            crate::services::mcp::channel_permissions::ChannelPermissionCallbacks::default();
        let (tx, rx) = async_channel::bounded(1);
        let _unsubscribe = callbacks.on_response("abcde", move |response| {
            tx.try_send(response).unwrap();
        });

        assert_eq!(
            handle_channel_permission_notification_event(
                Some(&callbacks),
                "telegram",
                "ABCDE",
                crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Allow,
                true,
            ),
            Some(true)
        );
        let response = rx.try_recv().unwrap();
        assert_eq!(
            response.behavior,
            crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Allow
        );
        assert_eq!(response.from_server, "telegram");
        assert_eq!(
            handle_channel_permission_notification_event(
                Some(&callbacks),
                "telegram",
                "abcde",
                crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Deny,
                true,
            ),
            Some(false)
        );
        assert_eq!(
            handle_channel_permission_notification_event(
                Some(&callbacks),
                "telegram",
                "abcde",
                crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Deny,
                false,
            ),
            None
        );
    }

    #[test]
    fn pending_mcp_state_from_configs_matches_official_initial_pending_clients() {
        with_isolated_config(|_| {
            let configs = indexmap::IndexMap::from([
                ("docs".to_string(), stdio_config("docs-mcp")),
                ("ide".to_string(), stdio_config("ide-mcp")),
            ]);
            let state = pending_mcp_state_from_configs(&configs);
            assert_eq!(state.clients.len(), 2);
            assert_eq!(state.clients[0].client.name, "docs");
            assert_eq!(
                state.clients[0].client.status,
                McpServerConnectionType::Pending
            );
            assert_eq!(
                state.clients[0]
                    .config
                    .as_ref()
                    .and_then(|config| config.command.as_deref()),
                Some("docs-mcp")
            );
            assert_eq!(state.clients[1].client.ide_name.as_deref(), Some("IDE"));
        });
    }

    #[test]
    fn pending_mcp_scope_merge_uses_official_object_entries_order() {
        // useManageMCPConnections.ts:777,817: spread retains ordinary key
        // positions, but Object.entries reorders array-index names after merging.
        with_isolated_config(|_| {
            let mut configs = indexmap::IndexMap::from([
                ("9".to_string(), stdio_config("nine")),
                ("z".to_string(), stdio_config("old")),
            ]);
            configs.extend([
                ("2".to_string(), stdio_config("two")),
                ("a".to_string(), stdio_config("alpha")),
                ("z".to_string(), stdio_config("new")),
            ]);
            let result = initialize_servers_as_pending(&McpState::default(), &configs);
            assert_eq!(result.new_clients, ["2", "9", "z", "a"]);
            assert_eq!(
                result
                    .state
                    .clients
                    .iter()
                    .map(|server| server.client.name.as_str())
                    .collect::<Vec<_>>(),
                ["2", "9", "z", "a"]
            );
            assert_eq!(
                result.state.clients[2]
                    .config
                    .as_ref()
                    .unwrap()
                    .command
                    .as_deref(),
                Some("new")
            );
        });
    }

    #[test]
    fn pending_mcp_state_marks_project_disabled_servers_without_connecting() {
        with_isolated_config(|temp_dir| {
            let project_dir = temp_dir.join("project");
            std::fs::create_dir_all(&project_dir).unwrap();
            let _cwd_guard = ProjectCwdGuard::enter(&project_dir);
            let current_project_dir = std::env::current_dir().unwrap();
            let project_key = crate::utils::config::normalize_project_path(
                &current_project_dir.to_string_lossy(),
            );
            let mut projects = serde_json::Map::new();
            projects.insert(
                project_key,
                serde_json::json!({
                    "disabledMcpServers": ["docs"]
                }),
            );
            std::fs::write(
                temp_dir.join(".claude.json"),
                serde_json::json!({ "projects": projects }).to_string(),
            )
            .unwrap();

            let configs =
                indexmap::IndexMap::from([("docs".to_string(), stdio_config("docs-mcp"))]);
            let state = pending_mcp_state_from_configs(&configs);
            assert_eq!(
                state.clients[0].client.status,
                McpServerConnectionType::Disabled
            );
        });
    }

    #[test]
    fn initialize_servers_as_pending_removes_stale_and_readds_changed_configs() {
        with_isolated_config(|_| {
            let mut removed_plugin = stdio_config("plugin-old");
            removed_plugin.scope = ConfigScope::Dynamic;
            let changed_old = stdio_config("docs-old");
            let changed_new = stdio_config("docs-new");
            let kept = stdio_config("kept");

            let current_state = McpState {
                clients: vec![
                    McpConnectionDiscovery::pending_with_config("removed-plugin", &removed_plugin)
                        .server,
                    McpConnectionDiscovery::pending_with_config("docs", &changed_old).server,
                    McpConnectionDiscovery::pending_with_config("kept", &kept).server,
                ],
                ..McpState::default()
            };
            let configs = indexmap::IndexMap::from([
                ("docs".to_string(), changed_new.clone()),
                ("kept".to_string(), kept.clone()),
                ("new".to_string(), stdio_config("new")),
            ]);

            let result = initialize_servers_as_pending(&current_state, &configs);
            assert_eq!(
                result
                    .stale
                    .iter()
                    .map(|server| server.client.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["removed-plugin", "docs"]
            );
            assert_eq!(
                result
                    .state
                    .clients
                    .iter()
                    .map(|server| server.client.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["kept", "docs", "new"]
            );
            let docs = result
                .state
                .clients
                .iter()
                .find(|server| server.client.name == "docs")
                .unwrap();
            assert_eq!(
                docs.config
                    .as_ref()
                    .and_then(|config| config.command.as_deref()),
                Some("docs-new")
            );
            assert_eq!(docs.client.status, McpServerConnectionType::Pending);
        });
    }

    #[test]
    fn reconnect_registry_cancels_only_source_timer_not_started_connection() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work().unwrap();
        let name = format!("reconnect-timer-{}", uuid::Uuid::new_v4());
        let (finish_connection, connection) = futures::channel::oneshot::channel();
        let (observed_send, observed) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            connection.await.unwrap();
            observed_send.send("connected").unwrap();
        });
        let timer = schedule_mcp_reconnect_timer(&name, 60_000);
        cancel_pending_mcp_reconnect(&name);
        assert!(
            futures::executor::block_on(timer).is_err(),
            "clearTimeout must never resume the retry loop"
        );
        finish_connection.send(()).unwrap();
        assert_eq!(
            observed
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            "connected",
            "already-started connection is independent of its earlier timer"
        );
        let timer = schedule_mcp_reconnect_timer(&name, 0);
        futures::executor::block_on(timer).unwrap();
        cancel_pending_mcp_reconnect(&name);
    }

    #[test]
    fn stale_mcp_cleanup_guard_matches_official_connected_only_cache_clear() {
        let config = stdio_config("docs-mcp");
        let mut connected = McpConnectionDiscovery::pending_with_config("docs", &config).server;
        connected.client.status = McpServerConnectionType::Connected;
        assert!(stale_mcp_server_needs_cache_cleanup(&connected));

        let mut pending = connected.clone();
        pending.client.status = McpServerConnectionType::Pending;
        assert!(!stale_mcp_server_needs_cache_cleanup(&pending));

        let mut failed = connected;
        failed.client.status = McpServerConnectionType::Failed;
        assert!(!stale_mcp_server_needs_cache_cleanup(&failed));
    }

    #[test]
    fn automatic_reconnect_helpers_match_official_remote_transport_policy() {
        assert_eq!(MAX_RECONNECT_ATTEMPTS, 5);
        assert_eq!(INITIAL_BACKOFF_MS, 1_000);
        assert_eq!(MAX_BACKOFF_MS, 30_000);
        assert!(!transport_supports_automatic_reconnect(Transport::Stdio));
        assert!(!transport_supports_automatic_reconnect(Transport::Sdk));
        assert!(transport_supports_automatic_reconnect(Transport::Sse));
        assert!(transport_supports_automatic_reconnect(Transport::Http));
        assert!(transport_supports_automatic_reconnect(Transport::Ws));
        assert_eq!(get_transport_display_name(Transport::Http), "HTTP");
        assert_eq!(get_transport_display_name(Transport::Ws), "WebSocket");
        assert_eq!(get_transport_display_name(Transport::WsIde), "WebSocket");
        assert_eq!(get_transport_display_name(Transport::Sse), "SSE");
        assert_eq!(reconnect_backoff_ms(1), 1_000);
        assert_eq!(reconnect_backoff_ms(2), 2_000);
        assert_eq!(reconnect_backoff_ms(5), 16_000);
        assert_eq!(reconnect_backoff_ms(6), 30_000);
        assert_eq!(
            closed_server_reconnect_decision(Transport::Http, true),
            ClosedServerReconnectDecision::SkipDisabled
        );
        assert_eq!(
            closed_server_reconnect_decision(Transport::Stdio, false),
            ClosedServerReconnectDecision::MarkFailed
        );
        assert_eq!(
            closed_server_reconnect_decision(Transport::Sdk, false),
            ClosedServerReconnectDecision::MarkFailed
        );
        assert_eq!(
            closed_server_reconnect_decision(Transport::Sse, false),
            ClosedServerReconnectDecision::StartAutomaticReconnect
        );
    }

    #[test]
    fn reconnect_pending_and_failed_updates_match_official_update_server_semantics() {
        let config = stdio_config("docs-mcp");
        let mut server = McpConnectionDiscovery::pending_with_config("docs", &config).server;
        server.client.status = McpServerConnectionType::Connected;
        server
            .tools
            .push(crate::services::mcp::types::McpToolSnapshot {
                name: "search".to_string(),
                display_name: None,
                description: None,
                input_schema: serde_json::json!({ "type": "object" }),
                read_only_hint: true,
                destructive_hint: false,
                open_world_hint: false,
            });
        server
            .prompts
            .push(crate::services::mcp::types::McpPromptSnapshot {
                name: "prompt".to_string(),
                description: None,
                arg_names: Vec::new(),
            });
        server
            .resources
            .push(crate::services::mcp::types::ServerResource {
                server: "docs".to_string(),
                uri: "docs://one".to_string(),
                name: "one".to_string(),
                description: None,
                mime_type: None,
            });

        let pending = pending_reconnect_server_update(&server, 2);
        assert_eq!(pending.client.status, McpServerConnectionType::Pending);
        assert_eq!(pending.client.reconnect_attempt, Some(2));
        assert_eq!(
            pending.client.max_reconnect_attempts,
            Some(MAX_RECONNECT_ATTEMPTS)
        );
        assert_eq!(pending.tools.len(), 1);
        assert_eq!(pending.prompts.len(), 1);
        assert_eq!(pending.resources.len(), 1);

        let failed = failed_reconnect_server_update(&server);
        assert_eq!(failed.client.status, McpServerConnectionType::Failed);
        assert_eq!(failed.client.reconnect_attempt, None);
        assert!(failed.tools.is_empty());
        assert!(failed.prompts.is_empty());
        assert!(failed.resources.is_empty());

        let cleared = clear_authentication_server_update(&server);
        assert_eq!(cleared.client.status, McpServerConnectionType::Failed);
        assert_eq!(cleared.client.reconnect_attempt, None);
        assert_eq!(cleared.client.max_reconnect_attempts, None);
        assert!(cleared.tools.is_empty());
        assert!(cleared.prompts.is_empty());
        assert!(cleared.resources.is_empty());
    }

    #[test]
    fn remote_authentication_messages_match_official_menu_copy() {
        assert_eq!(
            remote_authentication_result_message(McpServerConnectionType::Connected, "docs", false),
            "Authentication successful. Connected to docs."
        );
        assert_eq!(
            remote_authentication_result_message(McpServerConnectionType::Connected, "docs", true),
            "Authentication successful. Reconnected to docs."
        );
        assert_eq!(
            remote_authentication_result_message(McpServerConnectionType::NeedsAuth, "docs", false),
            "Authentication successful, but server still requires authentication. You may need to manually restart Claude Code."
        );
        assert_eq!(
            remote_authentication_result_message(McpServerConnectionType::Failed, "docs", false),
            "Authentication successful, but server reconnection failed. You may need to manually restart Claude Code for the changes to take effect."
        );
    }

    #[test]
    fn claude_ai_clear_update_matches_official_menu_state() {
        let mut server = McpConnectionDiscovery::pending_with_config(
            "claudeai",
            &claude_ai_proxy_config(Some("mcpsrv_abc")),
        )
        .server;
        server.client.status = McpServerConnectionType::Connected;
        server
            .tools
            .push(crate::services::mcp::types::McpToolSnapshot {
                name: "tool".to_string(),
                display_name: None,
                description: None,
                input_schema: serde_json::json!({"type":"object"}),
                read_only_hint: true,
                destructive_hint: false,
                open_world_hint: false,
            });
        let cleared = claude_ai_clear_authentication_server_update(&server);
        assert_eq!(cleared.client.status, McpServerConnectionType::NeedsAuth);
        assert!(cleared.tools.is_empty());
    }

    #[test]
    fn list_changed_update_helpers_match_official_update_server_semantics() {
        let config = stdio_config("docs-mcp");
        let mut server = McpConnectionDiscovery::pending_with_config("docs", &config).server;
        server.client.status = McpServerConnectionType::Connected;
        let old_tool = crate::services::mcp::types::McpToolSnapshot {
            name: "old-tool".to_string(),
            display_name: None,
            description: None,
            input_schema: serde_json::json!({ "type": "object" }),
            read_only_hint: true,
            destructive_hint: false,
            open_world_hint: false,
        };
        let old_prompt = crate::services::mcp::types::McpPromptSnapshot {
            name: "old-prompt".to_string(),
            description: None,
            arg_names: Vec::new(),
        };
        let old_resource = crate::services::mcp::types::ServerResource {
            server: "docs".to_string(),
            uri: "docs://old".to_string(),
            name: "old-resource".to_string(),
            description: None,
            mime_type: None,
        };
        server.tools = vec![old_tool.clone()];
        server.prompts = vec![old_prompt.clone()];
        server.resources = vec![old_resource.clone()];

        let new_tool = crate::services::mcp::types::McpToolSnapshot {
            name: "new-tool".to_string(),
            ..old_tool.clone()
        };
        let tool_update = tools_list_changed_server_update(&server, vec![new_tool.clone()]);
        assert_eq!(tool_update.tools, vec![new_tool]);
        assert_eq!(tool_update.prompts, vec![old_prompt.clone()]);
        assert_eq!(tool_update.resources, vec![old_resource.clone()]);
        assert_eq!(
            tool_update.client.status,
            McpServerConnectionType::Connected
        );

        let new_prompt = crate::services::mcp::types::McpPromptSnapshot {
            name: "new-prompt".to_string(),
            description: Some("fresh".to_string()),
            arg_names: vec!["arg".to_string()],
        };
        let prompt_update = prompts_list_changed_server_update(&server, vec![new_prompt.clone()]);
        assert_eq!(prompt_update.tools, vec![old_tool.clone()]);
        assert_eq!(prompt_update.prompts, vec![new_prompt]);
        assert_eq!(prompt_update.resources, vec![old_resource.clone()]);

        let new_resource = crate::services::mcp::types::ServerResource {
            server: "docs".to_string(),
            uri: "docs://new".to_string(),
            name: "new-resource".to_string(),
            description: Some("fresh".to_string()),
            mime_type: Some("text/plain".to_string()),
        };
        let resource_update =
            resources_list_changed_server_update(&server, vec![new_resource.clone()]);
        assert_eq!(resource_update.tools, vec![old_tool]);
        assert_eq!(resource_update.prompts, vec![old_prompt]);
        assert_eq!(resource_update.resources, vec![new_resource]);
    }

    #[test]
    fn apply_mcp_server_update_replaces_in_place_like_official_updated_clients() {
        let mut state = McpState {
            clients: vec![
                McpConnectionDiscovery::pending("alpha").server,
                McpConnectionDiscovery::pending("zeta").server,
            ],
            ..Default::default()
        };
        apply_mcp_server_update(
            &mut state,
            McpConnectionDiscovery::failed("alpha", &stdio_config("alpha"), "boom").server,
        );
        assert_eq!(state.clients[0].client.name, "alpha");
        assert_eq!(
            state.clients[0].client.status,
            McpServerConnectionType::Failed
        );
        assert_eq!(state.clients[1].client.name, "zeta");
    }

    #[test]
    fn server_update_replaces_only_its_flat_entries_and_appends_fresh_capabilities() {
        let mut alpha = McpConnectionDiscovery::pending("alpha").server;
        alpha.client.status = McpServerConnectionType::Connected;
        alpha
            .tools
            .push(crate::services::mcp::types::McpToolSnapshot {
                name: "old".to_string(),
                display_name: None,
                description: None,
                input_schema: serde_json::json!({"type":"object"}),
                read_only_hint: false,
                destructive_hint: false,
                open_world_hint: false,
            });
        alpha
            .prompts
            .push(crate::services::mcp::types::McpPromptSnapshot {
                name: "old-prompt".to_string(),
                description: None,
                arg_names: Vec::new(),
            });
        alpha
            .resources
            .push(crate::services::mcp::types::ServerResource {
                server: "alpha".to_string(),
                uri: "alpha://old".to_string(),
                name: "old".to_string(),
                description: None,
                mime_type: None,
            });

        let mut beta = McpConnectionDiscovery::pending("beta").server;
        beta.client.status = McpServerConnectionType::Connected;
        beta.tools
            .push(crate::services::mcp::types::McpToolSnapshot {
                name: "keep".to_string(),
                display_name: None,
                description: None,
                input_schema: serde_json::json!({"type":"object"}),
                read_only_hint: false,
                destructive_hint: false,
                open_world_hint: false,
            });
        beta.prompts
            .push(crate::services::mcp::types::McpPromptSnapshot {
                name: "keep-prompt".to_string(),
                description: None,
                arg_names: Vec::new(),
            });

        let mut state = McpState {
            clients: vec![alpha.clone(), beta],
            ..McpState::default()
        };
        crate::services::mcp::client::refresh_flat_mcp_capabilities(&mut state);
        alpha.tools[0].name = "new".to_string();
        alpha.prompts[0].name = "new-prompt".to_string();
        alpha.resources[0].uri = "alpha://new".to_string();
        apply_mcp_server_update(&mut state, alpha);

        assert_eq!(
            state
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__beta__keep", "mcp__alpha__new"]
        );
        assert_eq!(
            state
                .commands
                .iter()
                .map(|command| command.name.as_ref())
                .collect::<Vec<_>>(),
            vec!["mcp__beta__keep-prompt", "mcp__alpha__new-prompt"]
        );
        assert_eq!(state.resources["alpha"][0].uri, "alpha://new");
        assert_eq!(state.clients[0].client.name, "alpha");
        assert_eq!(state.clients[1].client.name, "beta");

        let commands_before_tools_patch = state
            .commands
            .iter()
            .map(|command| command.name.to_string())
            .collect::<Vec<_>>();
        // CC updateServer({ ...client, tools }): the update carries the full
        // captured client with the refreshed tools list.
        let mut alpha_tools_update = state.clients[0].clone();
        alpha_tools_update.tools = vec![crate::services::mcp::types::McpToolSnapshot {
            name: "newer".to_string(),
            display_name: None,
            description: None,
            input_schema: serde_json::json!({"type":"object"}),
            read_only_hint: false,
            destructive_hint: false,
            open_world_hint: false,
        }];
        apply_mcp_tools_list_changed(&mut state, alpha_tools_update);
        assert_eq!(
            state
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__beta__keep", "mcp__alpha__newer"]
        );
        assert_eq!(
            state
                .commands
                .iter()
                .map(|command| command.name.to_string())
                .collect::<Vec<_>>(),
            commands_before_tools_patch,
            "tools/list_changed must not move command entries"
        );
        assert_eq!(state.resources["alpha"][0].uri, "alpha://new");

        let tools_before_prompts_patch = state
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let mut alpha_prompts_update = state.clients[0].clone();
        alpha_prompts_update.prompts = vec![crate::services::mcp::types::McpPromptSnapshot {
            name: "newer-prompt".to_string(),
            description: None,
            arg_names: Vec::new(),
        }];
        apply_mcp_prompts_list_changed(&mut state, alpha_prompts_update);
        assert_eq!(
            state
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            tools_before_prompts_patch,
            "prompts/list_changed must not move tool entries"
        );
        assert_eq!(state.commands[1].name, "mcp__alpha__newer-prompt");

        let tools_before_pending = state.tools.clone();
        let commands_before_pending = state.commands.clone();
        let resources_before_pending = state.resources.clone();
        let pending = pending_reconnect_server_update(&state.clients[0], 1);
        apply_mcp_client_update(&mut state, pending);
        assert_eq!(
            state.clients[0].client.status,
            McpServerConnectionType::Pending
        );
        assert_eq!(state.tools, tools_before_pending);
        assert_eq!(state.commands, commands_before_pending);
        assert_eq!(state.resources, resources_before_pending);
    }

    #[test]
    fn list_changed_appends_missing_client_like_official_flush() {
        // CC flush `useManageMCPConnections.ts:250-253`: a client absent from
        // `mcp.clients` is APPENDED (`[...mcp.clients, client]`) — never
        // skipped. The update's other dimensions stay `undefined` (no
        // commands/resources contribution for the appended client).
        let mut existing = McpConnectionDiscovery::pending("alpha").server;
        existing.client.status = McpServerConnectionType::Connected;
        let mut state = McpState {
            clients: vec![existing],
            ..McpState::default()
        };
        crate::services::mcp::client::refresh_flat_mcp_capabilities(&mut state);

        let mut gamma = McpConnectionDiscovery::pending("gamma").server;
        gamma.client.status = McpServerConnectionType::Connected;
        gamma
            .tools
            .push(crate::services::mcp::types::McpToolSnapshot {
                name: "fresh".to_string(),
                display_name: None,
                description: None,
                input_schema: serde_json::json!({"type":"object"}),
                read_only_hint: false,
                destructive_hint: false,
                open_world_hint: false,
            });
        apply_mcp_tools_list_changed(&mut state, gamma);

        assert_eq!(
            state
                .clients
                .iter()
                .map(|server| server.client.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "gamma"],
            "missing client must be appended, preserving existing order"
        );
        assert!(
            state
                .tools
                .iter()
                .any(|tool| tool.name == "mcp__gamma__fresh")
        );
        let appended = &state.clients[1];
        assert!(appended.prompts.is_empty());
        assert!(appended.resources.is_empty());
        assert!(
            state.commands.is_empty(),
            "undefined dimensions must not contribute aggregates"
        );

        // prompts/list_changed for another unknown client appends likewise.
        let tools_before_delta = state.tools.clone();
        let mut delta = McpConnectionDiscovery::pending("delta").server;
        delta.client.status = McpServerConnectionType::Connected;
        delta
            .prompts
            .push(crate::services::mcp::types::McpPromptSnapshot {
                name: "fresh-prompt".to_string(),
                description: None,
                arg_names: Vec::new(),
            });
        apply_mcp_prompts_list_changed(&mut state, delta);
        assert_eq!(state.clients[2].client.name, "delta");
        assert!(
            state
                .commands
                .iter()
                .any(|command| command.name.as_ref() == "mcp__delta__fresh-prompt")
        );
        assert!(state.clients[2].tools.is_empty());
        assert!(state.clients[2].resources.is_empty());
        assert_eq!(
            state.tools, tools_before_delta,
            "prompts append must not move tool aggregates"
        );
        assert!(!state.resources.contains_key("delta"));

        // resources/list_changed appends and projects the resources map.
        let tools_before_epsilon = state.tools.clone();
        let commands_before_epsilon = state.commands.clone();
        let mut epsilon = McpConnectionDiscovery::pending("epsilon").server;
        epsilon.client.status = McpServerConnectionType::Connected;
        epsilon
            .resources
            .push(crate::services::mcp::types::ServerResource {
                server: "epsilon".to_string(),
                uri: "epsilon://fresh".to_string(),
                name: "fresh".to_string(),
                description: None,
                mime_type: None,
            });
        apply_mcp_resources_list_changed(&mut state, epsilon);
        assert_eq!(state.clients[3].client.name, "epsilon");
        assert_eq!(state.resources["epsilon"][0].uri, "epsilon://fresh");
        assert!(state.clients[3].tools.is_empty());
        assert!(state.clients[3].prompts.is_empty());
        assert_eq!(
            state.tools, tools_before_epsilon,
            "resources append must not move tool aggregates"
        );
        assert_eq!(
            state.commands, commands_before_epsilon,
            "resources append must not move command aggregates"
        );
    }

    #[test]
    fn pending_start_is_nonblocking_and_connection_updates_flat_capability_arrays() {
        let configs = indexmap::IndexMap::from([("docs".to_string(), stdio_config("docs"))]);
        let mut state = pending_mcp_state_from_configs(&configs);
        assert_eq!(state.clients.len(), 1);
        assert!(state.tools.is_empty());
        assert!(state.commands.is_empty());

        let mut connected =
            McpConnectionDiscovery::pending_with_config("docs", &configs["docs"]).server;
        connected.client.status = McpServerConnectionType::Connected;
        connected
            .tools
            .push(crate::services::mcp::types::McpToolSnapshot {
                name: "search".to_string(),
                display_name: None,
                description: Some("Search docs".to_string()),
                input_schema: serde_json::json!({"type": "object"}),
                read_only_hint: true,
                destructive_hint: false,
                open_world_hint: false,
            });
        connected
            .prompts
            .push(crate::services::mcp::types::McpPromptSnapshot {
                name: "summarize".to_string(),
                description: Some("Summarize docs".to_string()),
                arg_names: Vec::new(),
            });
        connected
            .resources
            .push(crate::services::mcp::types::ServerResource {
                server: "docs".to_string(),
                uri: "docs://guide".to_string(),
                name: "guide".to_string(),
                description: None,
                mime_type: Some("text/plain".to_string()),
            });
        apply_mcp_server_update(&mut state, connected);
        assert_eq!(state.tools[0].name, "mcp__docs__search");
        assert_eq!(state.commands[0].name, "mcp__docs__summarize");
        assert_eq!(state.resources["docs"][0].uri, "docs://guide");

        apply_mcp_server_update(
            &mut state,
            McpConnectionDiscovery::failed("docs", &configs["docs"], "offline").server,
        );
        assert!(state.tools.is_empty());
        assert!(state.commands.is_empty());
        // Actual source spreads old resources before omit(old, name), so the
        // failed update clears tool/command contributions but retains this key.
        assert_eq!(state.resources["docs"][0].uri, "docs://guide");
    }

    #[test]
    fn channel_message_notification_handler_gates_and_enqueues_like_official() {
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();
        let context = ChannelRuntimeGateContext {
            channels_enabled: true,
            has_claude_ai_oauth: true,
            subscription: Some("max".to_string()),
            policy_channels_enabled: None,
            session_channels: vec![crate::bootstrap::state::ChannelEntry::server(
                "planner", true,
            )],
            allowlist: crate::services::mcp::channel_notification::EffectiveChannelAllowlist {
                entries: Vec::new(),
                source: crate::services::mcp::channel_notification::ChannelAllowlistSource::Ledger,
            },
        };
        let mut meta = BTreeMap::new();
        meta.insert("thread_ts".to_string(), "1".to_string());
        let gate = handle_channel_message_notification_with_context(
            "planner",
            "hello",
            Some(&meta),
            true,
            None,
            &context,
        );
        assert_eq!(gate, ChannelGateResult::Register);
        let queue = crate::utils::message_queue_manager::get_command_queue();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].mode, "prompt");
        assert!(queue[0].is_meta);
        assert!(queue[0].skip_slash_commands);
        assert!(
            queue[0]
                .value
                .contains("<channel source=\"planner\" thread_ts=\"1\">")
        );

        crate::utils::message_queue_manager::clear_command_queue();
        let skipped = handle_channel_message_notification_with_context(
            "planner", "hello", None, false, None, &context,
        );
        assert!(matches!(
            skipped,
            ChannelGateResult::Skip {
                kind: crate::services::mcp::channel_notification::ChannelGateSkipKind::Capability,
                ..
            }
        ));
        assert!(crate::utils::message_queue_manager::get_command_queue().is_empty());
    }

    #[test]
    fn toggle_mcp_server_once_disables_connected_server_through_service_boundary() {
        with_isolated_config(|temp_dir| {
            let project_dir = temp_dir.join("project");
            std::fs::create_dir_all(&project_dir).unwrap();
            let _cwd_guard = ProjectCwdGuard::enter(&project_dir);
            let _session_write = EnvVarGuard::set("SESSION_WRITE_ENABLED", "1");
            let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

            let config = stdio_config("docs-mcp");
            let mut server = McpConnectionDiscovery::pending_with_config("docs", &config).server;
            server.client.status = McpServerConnectionType::Connected;
            let state = McpState {
                clients: vec![server],
                ..McpState::default()
            };

            let runtime = tokio::runtime::Runtime::new().unwrap();
            let updated = runtime
                .block_on(toggle_mcp_server_once("docs", &state, &config))
                .unwrap();

            assert_eq!(
                updated.server.client.status,
                McpServerConnectionType::Disabled
            );
            assert!(crate::services::mcp::config::is_mcp_server_disabled("docs"));
        });
    }
}
