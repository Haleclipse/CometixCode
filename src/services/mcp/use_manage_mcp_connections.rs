//! MCP connection manager boundary.
//! Maps to: CC `services/mcp/useManageMCPConnections.ts`.
//!
//! The React hook in CC reconciles configured MCP servers with AppState.mcp,
//! connects clients, refreshes tools/prompts/resources, and exposes reconnect
//! callbacks. Nyan keeps the same service boundary as async functions so the
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
    ConnectedMcpServer, McpServerConnection, McpServerConnectionType, ScopedMcpServerConfig,
    Transport,
};
use crate::state::app_state_store::{McpState, McpWriter};
use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::atomic::Ordering;
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

/// Maps to: CC `useManageMCPConnections.ts:168-200`: the channel permission
/// callbacks exist in a build with `KAIROS` or `KAIROS_CHANNELS`, and go into
/// `AppState.channelPermissionCallbacks` only while
/// `isChannelPermissionRelayEnabled()`. The callbacks live in
/// `AppState.channel_permission_callbacks`; this is the startup seed.
pub fn channel_permission_callbacks_from_official_gates()
-> Option<crate::services::mcp::channel_permissions::ChannelPermissionCallbacks> {
    use crate::utils::feature_flags::{FeatureFlag, feature_enabled};
    let channels_built =
        feature_enabled(FeatureFlag::Kairos) || feature_enabled(FeatureFlag::KairosChannels);
    let relay_enabled =
        crate::services::mcp::channel_permissions::is_channel_permission_relay_enabled();
    (channels_built && relay_enabled)
        .then(crate::services::mcp::channel_permissions::ChannelPermissionCallbacks::default)
}

/// Maps to: CC `services/mcp/useManageMCPConnections.ts#getTransportDisplayName`.
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
    server: &McpServerConnection,
    attempt: u32,
) -> McpServerConnection {
    McpServerConnection::Pending(super::types::PendingMcpServer {
        name: server.name().to_owned(),
        config: server.config().clone(),
        reconnect_attempt: Some(attempt),
        max_reconnect_attempts: Some(MAX_RECONNECT_ATTEMPTS),
    })
}

/// Maps to: CC `updateServer({ ...client, type: 'failed' })` reconnect final
/// failure branch. The flush clears a failed client's tools and commands; its
/// resources key stays (`useManageMCPConnections.ts:270-277`).
pub fn failed_reconnect_server_update(server: &McpServerConnection) -> McpServerConnection {
    McpServerConnection::failed(server.name(), server.config().clone(), None)
}

/// Maps to: CC `MCPRemoteServerMenu.tsx:443-469` `handleClearAuth`'s
/// `setAppState`, after the tokens are revoked and the cache cleared: the
/// client is marked failed ("a misnomer here") and loses its capabilities.
/// The existing client keeps its other fields (`clients.map(c => ... { ...c,
/// type })`, never an append), and its tools, commands and resources go
/// (`excludeToolsByServer`, `excludeCommandsByServer`,
/// `excludeResourcesByServer`).
pub fn clear_authentication_state(state: &mut McpState, server_name: &str) {
    for server in &mut state.clients {
        if server.name() == server_name {
            // `{ ...c, type: 'failed' }` keeps an error only a failed `c` had.
            let error = match server {
                McpServerConnection::Failed(failed) => failed.error.clone(),
                _ => None,
            };
            *server = McpServerConnection::failed(server.name(), server.config().clone(), error);
        }
    }
    state.tools = super::utils::exclude_tools_by_server(&state.tools, server_name);
    state.commands = super::utils::exclude_commands_by_server(&state.commands, server_name);
    state.resources = super::utils::exclude_resources_by_server(&state.resources, server_name);
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
        McpServerConnectionType::NeedsAuth => "Authentication successful, but server still requires authentication. You may need to manually restart nyan.".to_string(),
        _ => "Authentication successful, but server reconnection failed. You may need to manually restart nyan for the changes to take effect.".to_string(),
    }
}

/// Test-only authorization URL for [`authenticate_remote_mcp_server_once`].
#[cfg(test)]
pub(crate) static TEST_OAUTH_AUTHORIZATION_URL: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);

/// Maps to CC `MCPRemoteServerMenu.tsx#handleAuthenticate`'s OAuth half
/// (`:361-380`): preserve step-up OAuth state for re-auth, then run the MCP
/// OAuth service flow. The reconnect after it is the menu's, through the
/// connection manager (`:386` `reconnectMcpServer`), whose
/// `onConnectionAttempt` gives the new connection its handlers.
pub async fn authenticate_remote_mcp_server_once(
    server_name: &str,
    config: &ScopedMcpServerConfig,
    was_authenticated: bool,
    on_authorization_url: Option<super::auth::McpAuthorizationUrlCallback>,
    on_waiting_for_callback: Option<super::auth::McpWaitingForCallbackCallback>,
    abort_signal: Option<super::auth::McpOAuthAbortSignal>,
) -> anyhow::Result<()> {
    // The OAuth side-effect gate stops every real flow before it publishes an
    // authorization URL; tests stand in a flow that publishes one and waits.
    #[cfg(test)]
    let test_url = TEST_OAUTH_AUTHORIZATION_URL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    #[cfg(test)]
    if let Some(url) = test_url {
        if let Some(on_authorization_url) = &on_authorization_url {
            on_authorization_url(url);
        }
        return futures::future::pending().await;
    }
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
    Ok(())
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
) -> McpServerConnection {
    runtime_mcp
        .current()
        .clients
        .into_iter()
        .find(|server| server.name() == name)
        .unwrap_or_else(|| McpConnectionDiscovery::pending(name, config).server)
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

/// Maps to: CC `client.client.onclose` (`useManageMCPConnections.ts:333-468`):
/// a disabled server is left alone, stdio/sdk clients are marked failed, and
/// remote transports start `reconnectWithBackoff`.
pub async fn handle_mcp_server_closed_event(
    name: String,
    config: ScopedMcpServerConfig,
    runtime_mcp: McpWriter,
) {
    // CC's handler opens with `clearServerCache(client.name, client.config)`
    // (`useManageMCPConnections.ts:336`), unawaited. Assigning this handler
    // (`:333`) replaced `connectToServer`'s own `onclose` wrapper
    // (`client.ts:1374`), so for the REPL's connections that call is what
    // deletes the closed connection's memo entry and the name's fetch caches.
    // The close watcher running this `onclose` has already done both for
    // exactly that connection; clearing the key again here could only close a
    // newer connection made under it since.
    match closed_server_reconnect_decision(config.transport, is_mcp_server_disabled(&name)) {
        ClosedServerReconnectDecision::SkipDisabled => {
            crate::utils::log::log_mcp_debug(
                &name,
                "Server is disabled, skipping automatic reconnection",
            );
        }
        ClosedServerReconnectDecision::MarkFailed => {
            let base = reconnect_base_server(&name, &config, runtime_mcp.clone());
            runtime_mcp.apply_client_update(failed_reconnect_server_update(&base));
        }
        ClosedServerReconnectDecision::StartAutomaticReconnect => {
            crate::utils::log::log_mcp_debug(
                &name,
                &format!(
                    "{} transport closed/disconnected, attempting automatic reconnection",
                    get_transport_display_name(config.transport)
                ),
            );
            start_automatic_mcp_reconnect(name, config, runtime_mcp).await;
        }
    }
}

// The list_changed handlers below send the connection the list was refetched
// from, as CC's do with the `client` their closure captured
// (`updateServer({ ...client, <dimension> })`). That record is always
// connected, so the flush replaces the server's record with it even if it
// went pending or disabled meanwhile, and appends it if it is missing
// (`useManageMCPConnections.ts:232-253`).

/// Maps to: CC `ToolListChangedNotificationSchema` handler in
/// `services/mcp/useManageMCPConnections.ts#onConnectionAttempt`
/// (`updateServer({ ...client, tools: newTools })`, :656).
pub async fn handle_mcp_tools_list_changed_event(
    client: McpServerConnection,
    runtime_mcp: McpWriter,
) {
    let tools = refresh_mcp_tools_for_client(&client).await;
    runtime_mcp.apply_server_update(McpPendingUpdate {
        server: client,
        tools: Some(tools),
        commands: None,
        resources: None,
    });
}

/// Maps to: CC `PromptListChangedNotificationSchema` handler in
/// `services/mcp/useManageMCPConnections.ts#onConnectionAttempt`
/// (`updateServer({ ...client, commands: [...] })`, :688-691).
pub async fn handle_mcp_prompts_list_changed_event(
    client: McpServerConnection,
    runtime_mcp: McpWriter,
) {
    let commands = refresh_mcp_prompts_for_client(&client).await;
    runtime_mcp.apply_server_update(McpPendingUpdate {
        server: client,
        tools: None,
        commands: Some(commands),
        resources: None,
    });
}

/// Maps to: CC `ResourceListChangedNotificationSchema` handler
/// (`useManageMCPConnections.ts:705-751`). With `MCP_SKILLS`, the build CC
/// ships, the prompts are refetched alongside the resources and written
/// with them (`:718-738`); the MCP skill producer is a stub in the source
/// (`skills/mcpSkills.ts`), so its share of the commands is empty, and the
/// skill index clear is `EXPERIMENTAL_SKILL_SEARCH`'s, which is off.
pub async fn handle_mcp_resources_list_changed_event(
    client: McpServerConnection,
    runtime_mcp: McpWriter,
) {
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::McpSkills,
    ) {
        let (resources, commands) = tokio::join!(
            refresh_mcp_resources_for_client(&client),
            refresh_mcp_prompts_for_client(&client),
        );
        runtime_mcp.apply_server_update(McpPendingUpdate {
            server: client,
            tools: None,
            commands: Some(commands),
            resources: Some(resources),
        });
        return;
    }
    let resources = refresh_mcp_resources_for_client(&client).await;
    runtime_mcp.apply_server_update(McpPendingUpdate {
        server: client,
        tools: None,
        commands: None,
        resources: Some(resources),
    });
}

/// Maps to: CC `useManageMCPConnections.ts:470-614`, the channel block of
/// `onConnectionAttempt`: `gateChannelServer(client.name,
/// client.capabilities, client.config.pluginSource)` decides when the client
/// connects whether it gets channel handlers. Registered, its channel
/// messages are queued, and — if the server declared
/// `claude/channel/permission` — its permission replies resolve the
/// session's pending relays (`channelPermCallbacksRef.current`, here
/// `callbacks`, the same shared object). Skipped, any handlers an earlier
/// connect registered are removed, so a demotion takes effect.
///
/// CC also raises a toast once per skip kind (`:578-611`); it is not ported,
/// since nothing here reaches the notifications, and the block runs only
/// with `KAIROS` or `KAIROS_CHANNELS`, both off.
pub(crate) fn install_channel_handlers(
    connected: &ConnectedMcpServer,
    context: &ChannelRuntimeGateContext,
    callbacks: Option<ChannelPermissionCallbacks>,
) {
    use super::channel_notification::{
        CHANNEL_EXPERIMENTAL_CAPABILITY, CHANNEL_MESSAGE_NOTIFICATION_METHOD,
        CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY, CHANNEL_PERMISSION_METHOD,
        parse_channel_message_notification_params, parse_channel_permission_notification_params,
    };
    use futures::FutureExt as _;
    let declares = |capability: &str| {
        connected
            .capabilities
            .experimental
            .as_ref()
            .is_some_and(|experimental| experimental.contains_key(capability))
    };
    let gate = gate_channel_server_from_context(
        &connected.name,
        declares(CHANNEL_EXPERIMENTAL_CAPABILITY),
        context.channels_enabled,
        &context.session_channels,
        connected.config.plugin_source.as_deref(),
        &context.allowlist,
    );
    let client = &connected.client;
    match gate {
        ChannelGateResult::Register => {
            crate::utils::log::log_mcp_debug(&connected.name, "Channel notifications registered");
            let name = connected.name.clone();
            client.set_notification_handler(CHANNEL_MESSAGE_NOTIFICATION_METHOD, move |params| {
                match parse_channel_message_notification_params(params.as_ref()) {
                    Ok(parsed) => {
                        // CC `content.slice(0, 80)`: UTF-16 code units.
                        let preview = String::from_utf16_lossy(
                            &parsed.content.encode_utf16().take(80).collect::<Vec<_>>(),
                        );
                        crate::utils::log::log_mcp_debug(
                            &name,
                            &format!("notifications/claude/channel: {preview}"),
                        );
                        enqueue_channel_message_notification(
                            &name,
                            &parsed.content,
                            parsed.meta.as_ref(),
                        );
                    }
                    Err(error) => {
                        tracing::warn!(server = %name, error = %error, "invalid MCP channel notification");
                    }
                }
                futures::future::ready(()).boxed()
            });
            if declares(CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY) {
                let name = connected.name.clone();
                client.set_notification_handler(CHANNEL_PERMISSION_METHOD, move |params| {
                    match parse_channel_permission_notification_params(params.as_ref()) {
                        Ok(parsed) => {
                            let resolved =
                                super::channel_permissions::handle_channel_permission_notification(
                                    callbacks.as_ref(),
                                    &parsed.request_id,
                                    parsed.behavior,
                                    &name,
                                );
                            crate::utils::log::log_mcp_debug(
                                &name,
                                &format!(
                                    "notifications/claude/channel/permission: {} → {} ({})",
                                    parsed.request_id,
                                    parsed.behavior.as_str(),
                                    if resolved {
                                        "matched pending"
                                    } else {
                                        "no pending entry — stale or unknown ID"
                                    }
                                ),
                            );
                        }
                        Err(error) => {
                            tracing::warn!(server = %name, error = %error, "invalid MCP channel permission notification");
                        }
                    }
                    futures::future::ready(()).boxed()
                });
            }
        }
        ChannelGateResult::Skip { reason, .. } => {
            client.remove_notification_handler(CHANNEL_MESSAGE_NOTIFICATION_METHOD);
            client.remove_notification_handler(CHANNEL_PERMISSION_METHOD);
            crate::utils::log::log_mcp_debug(
                &connected.name,
                &format!("Channel notifications skipped: {reason}"),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// onConnectionAttempt handlers (CC useManageMCPConnections.ts)
//
// Claude Code registers `onclose` / `setNotificationHandler` closures inside
// `onConnectionAttempt` that call `updateServer` / `setAppState` directly.
// The elicitation, `onclose`, channel and list_changed handlers are assigned
// on the client itself here too ([`install_connection_handlers`]). App still
// mounts [`OnConnectionAttemptHandlers`] for what CC reaches through the
// session rather than a connection: the channel relay callbacks, and the
// queue push for a tool call's URL elicitation (`emit_elicitation_requested`).
// IDE `selection_changed` is REPL-owned (CC `useIdeSelection`): the REPL
// registers its handler on the IDE client itself
// (`hooks::use_ide_selection::use_ide_selection`).
// ---------------------------------------------------------------------------

/// Maps to: CC `onConnectionAttempt(result)` (`useManageMCPConnections.ts:
/// 310-763`) where the session writes the result at once — the automatic
/// reconnect, the manager's reconnect and enable: the connected client gets
/// its handlers, then `updateServer`. The initial connect batches the write
/// instead (`McpPendingUpdates::on_connection_attempt`).
///
/// Only the connections the session connects, reconnects or enables go
/// through it; an agent's, one `ensureConnectedClient` makes and one the
/// auth tool reconnects do not, and have no handlers.
pub(crate) fn on_connection_attempt(result: McpConnectionDiscovery, writer: &McpWriter) {
    install_connection_handlers(&result.server, writer);
    writer.apply_server_update(result);
}

/// Maps to: CC what `onConnectionAttempt` installs on a connected client
/// (`useManageMCPConnections.ts:326-753`): its elicitation handlers
/// (`registerElicitationHandler`, `:327-331`), its `onclose`
/// (`client.client.onclose = () => { ... }`, `:333-468`), the channel
/// handlers its gate allows (`:470-614`, [`install_channel_handlers`]), and a
/// list_changed handler per list the server declared can change
/// (`:616-751`). The handlers capture the writer where CC's capture
/// `updateServer`; the writer applies its updates at once, CC's queue
/// batches them.
fn install_connection_handlers(server: &McpServerConnection, writer: &McpWriter) {
    use crate::utils::feature_flags::{FeatureFlag, feature_enabled};
    use futures::FutureExt as _;
    use rmcp::model::ConstString as _;
    let Some(connected) = server.as_connected() else {
        return;
    };
    // Maps to: CC `:327-331`, in place of `connectToServer`'s default.
    super::elicitation_handler::register_elicitation_handler(
        &connected.client,
        &connected.name,
        writer,
    );
    {
        let (name, config, writer) = (
            connected.name.clone(),
            connected.config.clone(),
            writer.clone(),
        );
        connected.client.set_onclose(move || {
            handle_mcp_server_closed_event(name.clone(), config.clone(), writer.clone()).boxed()
        });
    }

    // Maps to: CC `:470-614`, built with `feature('KAIROS') ||
    // feature('KAIROS_CHANNELS')`.
    if feature_enabled(FeatureFlag::Kairos) || feature_enabled(FeatureFlag::KairosChannels) {
        install_channel_handlers(
            connected,
            &channel_gate_context_from_readonly_runtime(),
            with_connection_handlers(|hooks| hooks.channel_permission_callbacks.clone()).flatten(),
        );
    }

    // Maps to: CC `:616-751`: a handler for each list the server declared
    // can change, refreshing it on the client it closed over. The handler
    // lives on that client, so it holds it weakly.
    let capabilities = &connected.capabilities;
    let lists: [(bool, &str, &'static str, ListChangedHandler); 3] = [
        (
            declares_tools_list_changed(capabilities),
            rmcp::model::ToolListChangedNotificationMethod::VALUE,
            "Received tools/list_changed notification, refreshing tools",
            |client, writer| handle_mcp_tools_list_changed_event(client, writer).boxed(),
        ),
        (
            declares_prompts_list_changed(capabilities),
            rmcp::model::PromptListChangedNotificationMethod::VALUE,
            "Received prompts/list_changed notification, refreshing prompts",
            |client, writer| handle_mcp_prompts_list_changed_event(client, writer).boxed(),
        ),
        (
            declares_resources_list_changed(capabilities),
            rmcp::model::ResourceListChangedNotificationMethod::VALUE,
            "Received resources/list_changed notification, refreshing resources",
            |client, writer| handle_mcp_resources_list_changed_event(client, writer).boxed(),
        ),
    ];
    for (declared, method, message, refresh) in lists {
        if !declared {
            continue;
        }
        let (client, writer) = (connected.client.downgrade(), writer.clone());
        connected.client.set_notification_handler(method, move |_| {
            match client.connected_record() {
                Some(client) => {
                    crate::utils::log::log_mcp_debug(client.name(), message);
                    refresh(client, writer.clone())
                }
                None => futures::future::ready(()).boxed(),
            }
        });
    }
}

/// The body of a list_changed handler: refetch on `client`, write with it.
type ListChangedHandler =
    fn(McpServerConnection, McpWriter) -> futures::future::BoxFuture<'static, ()>;

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
}

fn connection_attempt_handlers_slot() -> &'static StdMutex<ConnectionAttemptRegistry> {
    CONNECTION_ATTEMPT_HANDLERS.get_or_init(|| StdMutex::new(ConnectionAttemptRegistry::default()))
}

/// The session-level handles the MCP layer reaches outside a connection's
/// own handlers: the channel relay callbacks (CC
/// `channelPermCallbacksRef`), and the writer for a tool call's URL
/// elicitation queue push (CC the caller's `setAppState`).
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

    /// Maps to: CC `callMCPToolWithUrlElicitationRetry`'s REPL branch
    /// (`client.ts:2948-2997`, the `setAppState` at `:2964-2995`): the queue
    /// push for a tool call's -32042 URL elicitation, which comes with the
    /// tool call's context rather than a connection's handler.
    pub fn on_elicitation_requested(&self, event: ElicitationRequestEvent) {
        self.mcp.push_elicitation_event(event);
    }
}

/// Maps to: CC mounting REPL/App with live `setAppState` / `updateServer` closures.
pub fn register_on_connection_attempt_handlers(hooks: OnConnectionAttemptHandlers) {
    connection_attempt_handlers_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .handlers = Some(hooks);
}

fn with_connection_handlers<R>(f: impl FnOnce(&OnConnectionAttemptHandlers) -> R) -> Option<R> {
    let guard = connection_attempt_handlers_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.handlers.as_ref().map(f)
}

// --- list_changed registration gates, and the URL elicitation queue push ---

/// Maps to: CC `useManageMCPConnections.ts:618`
/// `if (client.capabilities?.tools?.listChanged)`: it gates the handler's
/// registration — a server that never declared the capability has none
/// ([`install_connection_handlers`]).
pub(crate) fn declares_tools_list_changed(capabilities: &rmcp::model::ServerCapabilities) -> bool {
    capabilities
        .tools
        .as_ref()
        .and_then(|capability| capability.list_changed)
        .unwrap_or(false)
}

/// Maps to: CC `useManageMCPConnections.ts:667`
/// `if (client.capabilities?.prompts?.listChanged)`, the prompts handler's
/// registration gate.
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
/// `if (client.capabilities?.resources?.listChanged)`, the resources
/// handler's registration gate.
///
/// Note this is the `listChanged` sub-capability, NOT `!!capabilities.resources`
/// (CC `client.ts:2169`), which is a separate question: a connected record's
/// `capabilities.resources`.
pub(crate) fn declares_resources_list_changed(
    capabilities: &rmcp::model::ServerCapabilities,
) -> bool {
    capabilities
        .resources
        .as_ref()
        .and_then(|capability| capability.list_changed)
        .unwrap_or(false)
}

pub(crate) async fn emit_elicitation_requested(event: ElicitationRequestEvent) {
    if with_connection_handlers(|hooks| hooks.on_elicitation_requested(event.clone())).is_some() {
        return;
    }
    observe_for_test(McpConnectionCallbackObservation::ElicitationRequested { event }).await;
}

/// Observation queue used only when App has not registered session hooks
/// (unit tests that exercise the rmcp client without mounting App).
#[derive(Clone, Debug, PartialEq)]
pub enum McpConnectionCallbackObservation {
    ElicitationRequested { event: ElicitationRequestEvent },
}

static TEST_OBSERVATIONS: OnceLock<AsyncMutex<VecDeque<McpConnectionCallbackObservation>>> =
    OnceLock::new();

fn test_observations() -> &'static AsyncMutex<VecDeque<McpConnectionCallbackObservation>> {
    TEST_OBSERVATIONS.get_or_init(|| AsyncMutex::new(VecDeque::new()))
}

/// Queued in test builds only. A production run without App's hooks is a
/// print run, where CC hands a URL elicitation to the SDK host's
/// `handleElicitation` (`print.ts:2189`) instead of a queue: the event is
/// dropped there rather than kept unread.
async fn observe_for_test(observation: McpConnectionCallbackObservation) {
    #[cfg(test)]
    test_observations().lock().await.push_back(observation);
    #[cfg(not(test))]
    drop(observation);
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
/// `services/mcp/useManageMCPConnections.ts#onConnectionAttempt`
/// (`:371-464`), debug logs included.
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
            let transport_type = get_transport_display_name(config.transport);
            for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
                {
                    let _turn = crate::state::store::enter_store_turn_segment();
                    if is_mcp_server_disabled(&name) {
                        crate::utils::log::log_mcp_debug(
                            &name,
                            "Server disabled during reconnection, stopping retry",
                        );
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
                let reconnect_start = std::time::Instant::now();
                let result = reconnect_mcp_server_impl(&name, &config).await;
                let elapsed = reconnect_start.elapsed().as_millis();
                let resume = {
                    let _turn = crate::state::store::enter_store_turn_segment();
                    let status = result.server.connection_type();
                    if status == McpServerConnectionType::Connected {
                        crate::utils::log::log_mcp_debug(
                            &name,
                            &format!(
                                "{transport_type} reconnection successful after {elapsed}ms (attempt {attempt})"
                            ),
                        );
                    } else {
                        crate::utils::log::log_mcp_debug(
                            &name,
                            &format!(
                                "{transport_type} reconnection attempt {attempt} completed with status: {}",
                                status.as_str()
                            ),
                        );
                        if attempt == MAX_RECONNECT_ATTEMPTS {
                            crate::utils::log::log_mcp_debug(
                                &name,
                                &format!(
                                    "Max reconnection attempts ({MAX_RECONNECT_ATTEMPTS}) reached, giving up"
                                ),
                            );
                        }
                    }
                    if status == McpServerConnectionType::Connected
                        || attempt == MAX_RECONNECT_ATTEMPTS
                    {
                        RECONNECT_TASKS
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&name);
                        // CC `onConnectionAttempt(result)` (`:408`, `:424`).
                        on_connection_attempt(result, &runtime_mcp);
                        return;
                    }
                    let backoff_ms = reconnect_backoff_ms(attempt);
                    crate::utils::log::log_mcp_debug(
                        &name,
                        &format!(
                            "Scheduling reconnection attempt {} in {backoff_ms}ms",
                            attempt + 1
                        ),
                    );
                    schedule_mcp_reconnect_timer(&name, backoff_ms)
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
pub fn stale_mcp_server_needs_cache_cleanup(server: &McpServerConnection) -> bool {
    server.connection_type() == McpServerConnectionType::Connected
}

/// Maps to: CC `useManageMCPConnections.ts#initializeServersAsPending` stale
/// cleanup loop. CC sets `client.onclose = undefined` before `clearServerCache`;
/// The writer detaches the captured client identity before invoking this helper;
/// cache lookup occurs synchronously and cancellation completes asynchronously.
pub fn cleanup_stale_mcp_clients(
    stale: &[McpServerConnection],
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let cleanups = stale
        .iter()
        .filter(|server| stale_mcp_server_needs_cache_cleanup(server))
        .map(|server| clear_server_cache(server.name(), Some(server.config())))
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
    pub stale: Vec<McpServerConnection>,
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
        cancel_pending_mcp_reconnect(server.name());
    }
    let mut state = stale_result.state;
    let existing_names = state
        .clients
        .iter()
        .map(|server| server.name().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut new_clients = Vec::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(configs)
        .into_iter()
        .filter(|(name, _)| !existing_names.contains(*name))
    {
        let server = if is_mcp_server_disabled(name) {
            McpConnectionDiscovery::disabled(name, config).server
        } else {
            McpConnectionDiscovery::pending(name, config).server
        };
        new_clients.push(name.to_owned());
        // CC `:833` `clients: [...mcpWithoutStale.clients, ...newClients]`:
        // appended as they are, through no flush.
        state.clients.push(server);
    }
    InitializeServersAsPendingResult {
        state,
        stale: stale_result.stale,
        new_clients,
    }
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

    /// Maps to: CC `onConnectionAttempt` (`useManageMCPConnections.ts:
    /// 310-763`) for the initial connect: the connected client gets its
    /// handlers, and the result is batched into `updateServer`.
    fn on_connection_attempt(&self, discovery: McpConnectionDiscovery) {
        install_connection_handlers(&discovery.server, &self.writer);
        self.update_server(discovery);
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
            if let Some(expected) = expected_timer
                && state.timer.as_ref().map(|(generation, _)| *generation) != Some(expected)
            {
                return;
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
        super::config::get_claude_code_mcp_configs(&global, &project, &dependencies.startup.dynamic)
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
    // Maps to: CC `clearClaudeAIMcpConfigsCache`'s shared MCP auth-failure
    // cache invalidation, under its same non-strict guard.
    if !strict {
        super::client::clear_mcp_auth_cache();
    }
    let loaded = if strict {
        super::config::McpConfigs::default()
    } else {
        super::config::get_claude_code_mcp_configs(&global, &project, &dependencies.startup.dynamic)
            .await?
    };
    let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("MCP process runtime");
    let mut configs = loaded.servers;
    configs.extend(dependencies.startup.dynamic.clone());
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
    runtime.spawn(async move {
        get_mcp_tools_commands_and_resources(
            |discovery| pending.on_connection_attempt(discovery),
            &enabled,
        )
        .await;
    });
    Ok(())
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
    pub server: McpServerConnection,
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

/// Maps to: CC `useManageMCPConnections.ts:216-291#flushPendingUpdates`, the
/// body of its loop for one update.
pub fn apply_mcp_server_update(state: &mut McpState, update: impl Into<McpPendingUpdate>) {
    let McpPendingUpdate {
        server,
        mut tools,
        mut commands,
        mut resources,
    } = update.into();
    let name = server.name().to_owned();
    // `:232-243`: a disabled or failed client clears what it does not supply.
    if matches!(
        server.connection_type(),
        McpServerConnectionType::Disabled | McpServerConnectionType::Failed
    ) {
        tools.get_or_insert_with(Vec::new);
        commands.get_or_insert_with(Vec::new);
        resources.get_or_insert_with(Vec::new);
    }
    // `:255-268`: `undefined` keeps a dimension; otherwise the server's tools
    // (by prefix) and commands (by `commandBelongsToServer`) are replaced.
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
        .find(|client| client.name() == name)
    {
        *previous = server;
    } else {
        state.clients.push(server);
    }
}

/// Maps to: CC `updateServer({...client})`, with absent capability dimensions.
pub fn apply_mcp_client_update(state: &mut McpState, server: McpServerConnection) {
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

/// Maps to: CC `useMcpToggleEnabled()` / `toggleMcpServer`.
pub async fn toggle_mcp_server_once(
    name: &str,
    current_state: &McpState,
    config: &ScopedMcpServerConfig,
) -> anyhow::Result<McpConnectionDiscovery> {
    cancel_pending_mcp_reconnect(name);
    let server = current_state
        .clients
        .iter()
        .find(|server| server.name() == name)
        .ok_or_else(|| anyhow::anyhow!("MCP server {name} not found"))?;
    let is_currently_disabled = server.connection_type() == McpServerConnectionType::Disabled;

    if !is_currently_disabled {
        set_mcp_server_enabled(name, false)?;
        if server.connection_type() == McpServerConnectionType::Connected {
            clear_server_cache(name, Some(server.config())).await;
        }
        return Ok(McpConnectionDiscovery::disabled(name, config));
    }

    set_mcp_server_enabled(name, true)?;
    Ok(reconnect_mcp_server_impl(name, config).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mcp::types::{
        ConfigScope, ScopedMcpServerConfig, ServerResource, Transport,
    };
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
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcp-contract-review-0915/oracle.json"
        ))
        .unwrap();
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
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcp-contract-review-0915/oracle.json"
        ))
        .unwrap();
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
            let config = stdio_config("a");
            let server = if label == "failed" {
                McpServerConnection::failed("a", config, None)
            } else {
                McpServerConnection::pending("a", config)
            };
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
        let mut first = McpConnectionDiscovery::from(connected_server("a"));
        first.tools = vec![helper.clone()];
        apply_mcp_server_update(&mut state, first);
        let mut second = McpConnectionDiscovery::from(connected_server("b"));
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
        let mut repeated = McpConnectionDiscovery::pending("c", &stdio_config("c"));
        repeated.tools = vec![helper.clone()];
        apply_mcp_server_update(&mut state, repeated);
        assert_eq!(
            state.tools.iter().filter(|t| t.name == helper.name).count(),
            2,
            "interactive source does not uniqBy native helpers across invocations"
        );
        apply_mcp_client_update(
            &mut state,
            McpConnectionDiscovery::pending("b", &stdio_config("b")).server,
        );
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
                McpConnectionDiscovery::pending("slow", &stdio_config("slow")).server,
                McpConnectionDiscovery::pending("fast", &stdio_config("fast")).server,
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
                    pending.update_server(McpConnectionDiscovery::failed(
                        name,
                        &stdio_config(name),
                        "controlled",
                    ));
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
                first.clients[0].connection_type(),
                McpServerConnectionType::Pending
            );
            assert_eq!(
                first.clients[1].connection_type(),
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
            writer.current().clients[0].connection_type(),
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
                process_pending
                    .update_server(McpConnectionDiscovery::pending("z", &stdio_config("z")));
                process_pending
                    .update_server(McpConnectionDiscovery::pending("a", &stdio_config("a")));
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
                .map(|server| server.name())
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
        pending.update_server(McpConnectionDiscovery::pending("old", &stdio_config("old")));
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
        pending.update_server(McpConnectionDiscovery::pending("new", &stdio_config("new")));
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
                .map(|server| server.name())
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
                callback_pending.update_server(McpConnectionDiscovery::pending(
                    "reentrant",
                    &stdio_config("reentrant"),
                ));
            }
        }));
        let (finished, observed) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _turn = crate::state::store::enter_store_turn_segment();
            pending.update_server(McpConnectionDiscovery::pending(
                "first",
                &stdio_config("first"),
            ));
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
                    .map(|server| server.name())
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
            pending.update_server(McpConnectionDiscovery::pending(
                "delayed-poll",
                &stdio_config("delayed-poll"),
            ));
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
            plugin_source: None,
        }
    }

    fn with_isolated_config(test: impl FnOnce(&std::path::Path)) {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("nyan-mcp-manage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &temp_dir);
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
    fn initialize_servers_as_pending_from_empty_matches_official_initial_pending_clients() {
        with_isolated_config(|_| {
            let configs = indexmap::IndexMap::from([
                ("docs".to_string(), stdio_config("docs-mcp")),
                ("ide".to_string(), stdio_config("ide-mcp")),
            ]);
            let state = initialize_servers_as_pending(&McpState::default(), &configs).state;
            assert_eq!(state.clients.len(), 2);
            assert_eq!(state.clients[0].name(), "docs");
            assert_eq!(
                state.clients[0].connection_type(),
                McpServerConnectionType::Pending
            );
            assert_eq!(
                state.clients[0].config().command.as_deref(),
                Some("docs-mcp")
            );
            // CC's pending row is `{ name, type: 'pending', config }`: the
            // record keeps the config's `ideName` as is, and `/ide` supplies
            // the "IDE" label when it is absent.
            assert_eq!(state.clients[1].name(), "ide");
            assert_eq!(
                state.clients[1].connection_type(),
                McpServerConnectionType::Pending
            );
            assert_eq!(state.clients[1].config().ide_name, None);
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
                    .map(|server| server.name())
                    .collect::<Vec<_>>(),
                ["2", "9", "z", "a"]
            );
            assert_eq!(
                result.state.clients[2].config().command.as_deref(),
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
                temp_dir.join("state.json"),
                serde_json::json!({ "projects": projects }).to_string(),
            )
            .unwrap();

            let configs =
                indexmap::IndexMap::from([("docs".to_string(), stdio_config("docs-mcp"))]);
            let state = initialize_servers_as_pending(&McpState::default(), &configs).state;
            assert_eq!(
                state.clients[0].connection_type(),
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
                    McpConnectionDiscovery::pending("removed-plugin", &removed_plugin).server,
                    McpConnectionDiscovery::pending("docs", &changed_old).server,
                    McpConnectionDiscovery::pending("kept", &kept).server,
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
                    .map(|server| server.name())
                    .collect::<Vec<_>>(),
                vec!["removed-plugin", "docs"]
            );
            assert_eq!(
                result
                    .state
                    .clients
                    .iter()
                    .map(|server| server.name())
                    .collect::<Vec<_>>(),
                vec!["kept", "docs", "new"]
            );
            let docs = result
                .state
                .clients
                .iter()
                .find(|server| server.name() == "docs")
                .unwrap();
            assert_eq!(docs.config().command.as_deref(), Some("docs-new"));
            assert_eq!(docs.connection_type(), McpServerConnectionType::Pending);
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
        let connected = McpServerConnection::connected_for_test("docs", config.clone());
        assert!(stale_mcp_server_needs_cache_cleanup(&connected));

        let pending = McpServerConnection::pending("docs", config.clone());
        assert!(!stale_mcp_server_needs_cache_cleanup(&pending));

        let failed = McpServerConnection::failed("docs", config, None);
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

    /// An `mcp.tools` entry for `server`, as `fetchToolsForClient` makes it.
    fn flat_tool(server: &str, name: &str) -> crate::types::tools::Tool {
        crate::services::mcp::client::mcp_tool_for_test(
            server,
            serde_json::json!({"name": name, "inputSchema": {"type": "object"}}),
        )
    }

    /// An `mcp.commands` entry for `server`, as `fetchCommandsForClient`
    /// makes it.
    fn flat_command(server: &str, prompt: &str) -> crate::commands::Command {
        crate::services::mcp::client::mcp_command_for_test(server, prompt, None, &[])
    }

    fn flat_resource(server: &str, uri: &str) -> ServerResource {
        ServerResource {
            server: server.to_string(),
            uri: uri.to_string(),
            name: uri.to_string(),
            description: None,
            mime_type: None,
        }
    }

    fn connected_server(name: &str) -> McpServerConnection {
        McpServerConnection::connected_for_test(name, stdio_config(name))
    }

    fn server_update(
        server: McpServerConnection,
        tools: Option<Vec<crate::types::tools::Tool>>,
        commands: Option<Vec<crate::commands::Command>>,
        resources: Option<Vec<ServerResource>>,
    ) -> McpPendingUpdate {
        McpPendingUpdate {
            server,
            tools,
            commands,
            resources,
        }
    }

    fn tool_names(state: &McpState) -> Vec<&str> {
        state.tools.iter().map(|tool| tool.name.as_str()).collect()
    }

    fn command_names(state: &McpState) -> Vec<&str> {
        state
            .commands
            .iter()
            .map(|command| command.name.as_ref())
            .collect()
    }

    /// `docs` connected with one tool, prompt command and resource.
    fn docs_state() -> McpState {
        let mut state = McpState::default();
        apply_mcp_server_update(
            &mut state,
            server_update(
                connected_server("docs"),
                Some(vec![flat_tool("docs", "search")]),
                Some(vec![flat_command("docs", "prompt")]),
                Some(vec![flat_resource("docs", "docs://one")]),
            ),
        );
        state
    }

    /// CC's reconnect loop writes `{ ...client, type: 'pending',
    /// reconnectAttempt, maxReconnectAttempts }` and, on giving up,
    /// `{ ...client, type: 'failed' }`, both through `updateServer` with no
    /// capability dimensions. The flush keeps a pending client's capabilities
    /// and clears a failed one's tools and commands; its resources key stays
    /// (`useManageMCPConnections.ts:270-277` spreads the old map before
    /// `omit(old, name)`).
    #[test]
    fn reconnect_pending_and_failed_updates_match_official_update_server_semantics() {
        let mut state = docs_state();
        let server = state.clients[0].clone();

        // `reconnectAttempt` / `maxReconnectAttempts`, which only a pending
        // record carries.
        let reconnect_attempts = |server: &McpServerConnection| match server {
            McpServerConnection::Pending(pending) => {
                (pending.reconnect_attempt, pending.max_reconnect_attempts)
            }
            _ => (None, None),
        };

        let pending = pending_reconnect_server_update(&server, 2);
        assert_eq!(pending.connection_type(), McpServerConnectionType::Pending);
        assert_eq!(
            reconnect_attempts(&pending),
            (Some(2), Some(MAX_RECONNECT_ATTEMPTS))
        );
        apply_mcp_client_update(&mut state, pending);
        assert_eq!(tool_names(&state), vec!["mcp__docs__search"]);
        assert_eq!(command_names(&state), vec!["mcp__docs__prompt"]);
        assert_eq!(state.resources["docs"].len(), 1);

        let failed = failed_reconnect_server_update(&server);
        assert_eq!(failed.connection_type(), McpServerConnectionType::Failed);
        assert_eq!(reconnect_attempts(&failed), (None, None));
        apply_mcp_client_update(&mut state, failed);
        assert_eq!(
            state.clients[0].connection_type(),
            McpServerConnectionType::Failed
        );
        assert!(state.tools.is_empty());
        assert!(state.commands.is_empty());
        assert_eq!(state.resources["docs"].len(), 1);
    }

    /// The flush's `rawX ?? []` (`useManageMCPConnections.ts:232-243`) only
    /// fills an absent dimension: a disabled or failed update that supplies
    /// one writes what it supplies.
    #[test]
    fn disabled_and_failed_updates_write_the_dimensions_they_supply() {
        let mut state = docs_state();
        let failed = McpServerConnection::failed("docs", stdio_config("docs"), None);
        apply_mcp_server_update(
            &mut state,
            server_update(
                failed,
                Some(vec![flat_tool("docs", "authenticate")]),
                None,
                Some(vec![flat_resource("docs", "docs://two")]),
            ),
        );
        assert_eq!(tool_names(&state), vec!["mcp__docs__authenticate"]);
        assert!(state.commands.is_empty());
        assert_eq!(state.resources["docs"][0].uri, "docs://two");

        let disabled = McpServerConnection::disabled("docs", stdio_config("docs"));
        apply_mcp_server_update(
            &mut state,
            server_update(
                disabled,
                None,
                Some(vec![flat_command("docs", "kept")]),
                None,
            ),
        );
        assert!(state.tools.is_empty());
        assert_eq!(command_names(&state), vec!["mcp__docs__kept"]);
        assert_eq!(state.resources["docs"][0].uri, "docs://two");
    }

    /// CC `MCPRemoteServerMenu.tsx:443-469` writes `AppState.mcp` directly:
    /// the existing client changes type (never appended), and
    /// `exclude*ByServer` drop its tools, commands and resources key — unlike
    /// the flush, which keeps the key.
    #[test]
    fn clear_auth_state_matches_official_menu_set_app_state() {
        let mut state = docs_state();
        apply_mcp_server_update(
            &mut state,
            server_update(
                connected_server("other"),
                Some(vec![flat_tool("other", "keep")]),
                Some(vec![flat_command("other", "keep")]),
                Some(vec![flat_resource("other", "other://keep")]),
            ),
        );

        clear_authentication_state(&mut state, "docs");
        assert_eq!(
            state.clients[0].connection_type(),
            McpServerConnectionType::Failed
        );
        assert_eq!(tool_names(&state), vec!["mcp__other__keep"]);
        assert_eq!(command_names(&state), vec!["mcp__other__keep"]);
        assert!(!state.resources.contains_key("docs"));
        assert!(state.resources.contains_key("other"));

        clear_authentication_state(&mut state, "absent");
        assert_eq!(state.clients.len(), 2, "`clients.map` never appends");
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
            "Authentication successful, but server still requires authentication. You may need to manually restart nyan."
        );
        assert_eq!(
            remote_authentication_result_message(McpServerConnectionType::Failed, "docs", false),
            "Authentication successful, but server reconnection failed. You may need to manually restart nyan for the changes to take effect."
        );
    }

    #[test]
    fn apply_mcp_server_update_replaces_in_place_like_official_updated_clients() {
        let mut state = McpState {
            clients: vec![
                McpConnectionDiscovery::pending("alpha", &stdio_config("alpha")).server,
                McpConnectionDiscovery::pending("zeta", &stdio_config("zeta")).server,
            ],
            ..Default::default()
        };
        apply_mcp_server_update(
            &mut state,
            McpConnectionDiscovery::failed("alpha", &stdio_config("alpha"), "boom"),
        );
        assert_eq!(state.clients[0].name(), "alpha");
        assert_eq!(
            state.clients[0].connection_type(),
            McpServerConnectionType::Failed
        );
        assert_eq!(state.clients[1].name(), "zeta");
    }

    #[test]
    fn server_update_replaces_only_its_flat_entries_and_appends_fresh_capabilities() {
        let mut state = McpState::default();
        apply_mcp_server_update(
            &mut state,
            server_update(
                connected_server("alpha"),
                Some(vec![flat_tool("alpha", "old")]),
                Some(vec![flat_command("alpha", "old-prompt")]),
                Some(vec![flat_resource("alpha", "alpha://old")]),
            ),
        );
        apply_mcp_server_update(
            &mut state,
            server_update(
                connected_server("beta"),
                Some(vec![flat_tool("beta", "keep")]),
                Some(vec![flat_command("beta", "keep-prompt")]),
                None,
            ),
        );
        apply_mcp_server_update(
            &mut state,
            server_update(
                connected_server("alpha"),
                Some(vec![flat_tool("alpha", "new")]),
                Some(vec![flat_command("alpha", "new-prompt")]),
                Some(vec![flat_resource("alpha", "alpha://new")]),
            ),
        );

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
        assert_eq!(state.clients[0].name(), "alpha");
        assert_eq!(state.clients[1].name(), "beta");

        let commands_before_tools_patch = state
            .commands
            .iter()
            .map(|command| command.name.to_string())
            .collect::<Vec<_>>();
        // CC updateServer({ ...client, tools }): the captured client with the
        // refreshed tools, every other dimension absent.
        let alpha = state.clients[0].clone();
        apply_mcp_server_update(
            &mut state,
            server_update(alpha, Some(vec![flat_tool("alpha", "newer")]), None, None),
        );
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
        let alpha = state.clients[0].clone();
        apply_mcp_server_update(
            &mut state,
            server_update(
                alpha,
                None,
                Some(vec![flat_command("alpha", "newer-prompt")]),
                None,
            ),
        );
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
            state.clients[0].connection_type(),
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
        let mut state = McpState::default();
        apply_mcp_client_update(&mut state, connected_server("alpha"));

        apply_mcp_server_update(
            &mut state,
            server_update(
                connected_server("gamma"),
                Some(vec![flat_tool("gamma", "fresh")]),
                None,
                None,
            ),
        );

        assert_eq!(
            state
                .clients
                .iter()
                .map(|server| server.name())
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
        assert!(
            state.commands.is_empty(),
            "undefined dimensions must not contribute aggregates"
        );
        assert!(!state.resources.contains_key("gamma"));

        // prompts/list_changed for another unknown client appends likewise.
        let tools_before_delta = state.tools.clone();
        apply_mcp_server_update(
            &mut state,
            server_update(
                connected_server("delta"),
                None,
                Some(vec![flat_command("delta", "fresh-prompt")]),
                None,
            ),
        );
        assert_eq!(state.clients[2].name(), "delta");
        assert!(
            state
                .commands
                .iter()
                .any(|command| command.name.as_ref() == "mcp__delta__fresh-prompt")
        );
        assert_eq!(
            state.tools, tools_before_delta,
            "prompts append must not move tool aggregates"
        );
        assert!(!state.resources.contains_key("delta"));

        // resources/list_changed appends and writes the resources map.
        let tools_before_epsilon = state.tools.clone();
        let commands_before_epsilon = state.commands.clone();
        apply_mcp_server_update(
            &mut state,
            server_update(
                connected_server("epsilon"),
                None,
                None,
                Some(vec![flat_resource("epsilon", "epsilon://fresh")]),
            ),
        );
        assert_eq!(state.clients[3].name(), "epsilon");
        assert_eq!(state.resources["epsilon"][0].uri, "epsilon://fresh");
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
        let mut state = initialize_servers_as_pending(&McpState::default(), &configs).state;
        assert_eq!(state.clients.len(), 1);
        assert!(state.tools.is_empty());
        assert!(state.commands.is_empty());

        // CC `onConnectionAttempt({ client, tools, commands, resources })`.
        let connected = McpServerConnection::connected_for_test("docs", configs["docs"].clone());
        apply_mcp_server_update(
            &mut state,
            server_update(
                connected,
                Some(vec![flat_tool("docs", "search")]),
                Some(vec![flat_command("docs", "summarize")]),
                Some(vec![flat_resource("docs", "docs://guide")]),
            ),
        );
        assert_eq!(state.tools[0].name, "mcp__docs__search");
        assert_eq!(state.commands[0].name, "mcp__docs__summarize");
        assert_eq!(state.resources["docs"][0].uri, "docs://guide");

        apply_mcp_server_update(
            &mut state,
            McpConnectionDiscovery::failed("docs", &configs["docs"], "offline"),
        );
        assert!(state.tools.is_empty());
        assert!(state.commands.is_empty());
        // Actual source spreads old resources before omit(old, name), so the
        // failed update clears tool/command contributions but retains this key.
        assert_eq!(state.resources["docs"][0].uri, "docs://guide");
    }

    #[test]
    fn toggle_mcp_server_once_disables_connected_server_through_service_boundary() {
        with_isolated_config(|temp_dir| {
            let project_dir = temp_dir.join("project");
            std::fs::create_dir_all(&project_dir).unwrap();
            let _cwd_guard = ProjectCwdGuard::enter(&project_dir);
            let _session_write = EnvVarGuard::set("SESSION_WRITE_ENABLED", "1");
            let _write = EnvVarGuard::set("NYAN_WRITE_ENABLED", "1");

            let config = stdio_config("docs-mcp");
            let server = McpServerConnection::connected_for_test("docs", config.clone());
            let state = McpState {
                clients: vec![server],
                ..McpState::default()
            };

            let runtime = tokio::runtime::Runtime::new().unwrap();
            let updated = runtime
                .block_on(toggle_mcp_server_once("docs", &state, &config))
                .unwrap();

            assert_eq!(
                updated.server.connection_type(),
                McpServerConnectionType::Disabled
            );
            assert!(crate::services::mcp::config::is_mcp_server_disabled("docs"));
        });
    }

    /// Records every `log_mcp_debug` line as `debug:<server>:<message>`.
    fn record_mcp_debug_logs() -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        use std::sync::Arc;
        crate::utils::log::_reset_error_log_for_testing();
        let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let debug = logs.clone();
        crate::utils::log::attach_error_log_sink(crate::utils::log::ErrorLogSink {
            log_error: Arc::new(|_| Ok(())),
            log_mcp_error: Arc::new(|_, _| Ok(())),
            log_mcp_debug: Arc::new(move |server, message| {
                debug
                    .lock()
                    .unwrap()
                    .push(format!("debug:{server}:{message}"));
                Ok(())
            }),
            get_errors_path: Arc::new(|| Ok(std::path::PathBuf::new())),
            get_mcp_logs_path: Arc::new(|_| Ok(std::path::PathBuf::new())),
        })
        .unwrap();
        logs
    }

    fn wait_for_log(logs: &std::sync::Mutex<Vec<String>>, line: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !logs.lock().unwrap().iter().any(|logged| logged == line) {
            assert!(
                std::time::Instant::now() < deadline,
                "never logged {line:?}; logged {:#?}",
                logs.lock().unwrap()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The debug lines CC's `onclose` and `reconnectWithBackoff` write
    /// (`useManageMCPConnections.ts:346-453`): the remote transport closing,
    /// an attempt's outcome, the next attempt's backoff, a server disabled
    /// mid-retry, and a disabled server that closes; and neither a success
    /// nor a give-up line for a failed attempt that is not the last.
    #[test]
    fn closed_remote_server_logs_its_automatic_reconnection_like_official() {
        with_isolated_config(|temp_dir| {
            let project_dir = temp_dir.join("project");
            std::fs::create_dir_all(&project_dir).unwrap();
            let _cwd_guard = ProjectCwdGuard::enter(&project_dir);
            let _session_write = EnvVarGuard::set("SESSION_WRITE_ENABLED", "1");
            let _write = EnvVarGuard::set("NYAN_WRITE_ENABLED", "1");
            crate::utils::process_runtime::initialize_test_process_runtime();
            let logs = record_mcp_debug_logs();

            // The server holds the first connection it gets until the test
            // lets go, then returns a deterministic non-auth failure. Later
            // connections are dropped; the first attempt fails only once the
            // server has been disabled, whatever the load or socket platform.
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (accepted, first_accepted) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                use std::io::Write as _;
                let mut first = true;
                for mut stream in listener.incoming().flatten() {
                    if std::mem::take(&mut first) {
                        let _ = accepted.send(());
                        let _ = released.recv();
                        let _ = stream.write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            });

            let name = "docs-remote";
            let mut config = stdio_config("unused");
            config.transport = Transport::Http;
            config.command = None;
            config.url = Some(format!("http://127.0.0.1:{port}/mcp"));
            let writer = McpWriter::new(crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            ));
            let line = |message: &str| format!("debug:{name}:{message}");
            let runtime = tokio::runtime::Runtime::new().unwrap();

            runtime.block_on(handle_mcp_server_closed_event(
                name.to_string(),
                config.clone(),
                writer.clone(),
            ));
            wait_for_log(
                &logs,
                &line("HTTP transport closed/disconnected, attempting automatic reconnection"),
            );

            // Disabled while the first attempt is in flight: it still
            // completes, and the loop stops before the second.
            first_accepted
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the first attempt connects");
            set_mcp_server_enabled(name, false).unwrap();
            release.send(()).unwrap();
            wait_for_log(
                &logs,
                &line("HTTP reconnection attempt 1 completed with status: failed"),
            );
            wait_for_log(&logs, &line("Scheduling reconnection attempt 2 in 1000ms"));
            wait_for_log(
                &logs,
                &line("Server disabled during reconnection, stopping retry"),
            );
            for absent in [
                "attempt 2 completed",
                "(attempt 2)",
                "reconnection successful",
                "Max reconnection attempts",
            ] {
                assert!(
                    !logs
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|logged| logged.contains(absent)),
                    "logged {absent:?}: {:#?}",
                    logs.lock().unwrap()
                );
            }

            runtime.block_on(handle_mcp_server_closed_event(
                name.to_string(),
                config,
                writer,
            ));
            wait_for_log(
                &logs,
                &line("Server is disabled, skipping automatic reconnection"),
            );
        });
    }
}
