//! Maps to: CC `components/mcp/MCPRemoteServerMenu.tsx`.
//!
//! OAuth protocol details live in `services/mcp/auth.rs`; this menu retains
//! source-owned URL selection/browser dispatch and renders runtime snapshots.

use super::capabilities_section::CapabilitiesSection;
use super::types::{ServerInfo, mcp_client_state_from_parts};
use super::utils::reconnect_helpers::handle_reconnect_result;
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::keyboard_shortcut_hint::{
    KeyboardShortcutHint, KeyboardShortcutHintStyleContext,
};
use crate::components::spinner::Spinner;
use crate::constants::figures;
use crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings;
use crate::services::mcp::types::{McpServerConnectionType, Transport};
use crate::services::mcp::utils::describe_mcp_config_file_path;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteServerMenuAction {
    Enable,
    ViewTools,
    Authenticate,
    ReAuthenticate,
    ClearAuthentication,
    ClaudeAiAuthenticate,
    ClaudeAiClearAuthentication,
    Reconnect,
    Disable,
    Back,
}

#[derive(Default, Props)]
pub struct MCPRemoteServerMenuProps<'a> {
    pub server: Option<ServerInfo>,
    pub on_view_tools: HandlerMut<'a, ()>,
    pub on_cancel: HandlerMut<'a, ()>,
    pub on_complete: Handler<String>,
    pub borderless: bool,
}

/// Maps to: CC `MCPRemoteServerMenu.tsx` unmount cleanup that aborts
/// `authAbortControllerRef.current` so the OAuth callback server is closed.
#[derive(Default)]
struct RemoteOAuthAbortCleanup {
    handle:
        std::sync::Arc<std::sync::Mutex<Option<crate::services::mcp::auth::McpOAuthAbortHandle>>>,
}

impl Hook for RemoteOAuthAbortCleanup {}

impl Drop for RemoteOAuthAbortCleanup {
    fn drop(&mut self) {
        if let Ok(mut handle) = self.handle.lock() {
            if let Some(handle) = handle.take() {
                handle.abort();
            }
        }
    }
}

fn capitalize(input: &str) -> String {
    let mut chars = input.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn is_effectively_authenticated(server: &ServerInfo) -> bool {
    server.is_authenticated.unwrap_or(false)
        || (server.official_client_type() == McpServerConnectionType::Connected
            && !server.tools.is_empty())
}

pub fn menu_actions_for_remote_server(server: &ServerInfo) -> Vec<RemoteServerMenuAction> {
    let mut actions = Vec::new();
    let is_authenticated = is_effectively_authenticated(server);
    let is_claude_ai = server.transport == Transport::ClaudeAiProxy;

    if server.official_client_type() == McpServerConnectionType::Disabled {
        actions.push(RemoteServerMenuAction::Enable);
    }

    if server.official_client_type() == McpServerConnectionType::Connected
        && !server.tools.is_empty()
    {
        actions.push(RemoteServerMenuAction::ViewTools);
    }

    if is_claude_ai {
        if server.official_client_type() == McpServerConnectionType::Connected {
            actions.push(RemoteServerMenuAction::ClaudeAiClearAuthentication);
        } else if server.official_client_type() != McpServerConnectionType::Disabled {
            actions.push(RemoteServerMenuAction::ClaudeAiAuthenticate);
        }
    } else if is_authenticated {
        actions.push(RemoteServerMenuAction::ReAuthenticate);
        actions.push(RemoteServerMenuAction::ClearAuthentication);
    } else {
        actions.push(RemoteServerMenuAction::Authenticate);
    }

    if server.official_client_type() != McpServerConnectionType::Disabled {
        if server.official_client_type() != McpServerConnectionType::NeedsAuth {
            actions.push(RemoteServerMenuAction::Reconnect);
        }
        actions.push(RemoteServerMenuAction::Disable);
    }

    if actions.is_empty() {
        actions.push(RemoteServerMenuAction::Back);
    }
    actions
}

fn option_for_action(action: RemoteServerMenuAction) -> SelectOptionData {
    let label = match action {
        RemoteServerMenuAction::Enable => "Enable",
        RemoteServerMenuAction::ViewTools => "View tools",
        RemoteServerMenuAction::Authenticate | RemoteServerMenuAction::ClaudeAiAuthenticate => {
            "Authenticate"
        }
        RemoteServerMenuAction::ReAuthenticate => "Re-authenticate",
        RemoteServerMenuAction::ClearAuthentication
        | RemoteServerMenuAction::ClaudeAiClearAuthentication => "Clear authentication",
        RemoteServerMenuAction::Reconnect => "Reconnect",
        RemoteServerMenuAction::Disable => "Disable",
        RemoteServerMenuAction::Back => "Back",
    };
    SelectOptionData {
        label: label.to_string(),
        value: format!("{:?}", action),
        description: None,
        dim_description: false,
        disabled: false,
        input: None,
    }
}

fn status_text(status: McpServerConnectionType) -> (&'static str, &'static str) {
    let figures = figures::get();
    match status {
        McpServerConnectionType::Disabled => (figures.radio_off, "disabled"),
        McpServerConnectionType::Connected => (figures.tick, "connected"),
        McpServerConnectionType::Pending => (figures.radio_off, "connecting…"),
        McpServerConnectionType::NeedsAuth => (figures.triangle_up_outline, "needs authentication"),
        McpServerConnectionType::Failed => (figures.cross, "failed"),
    }
}

/// Maps to CC `MCPRemoteServerMenu.tsx` auth URL copy hint shown next to
/// fallback links.
pub fn auth_url_copy_hint(copied: bool) -> &'static str {
    if copied { "(Copied!)" } else { "(c copy)" }
}

/// Maps to: CC `components/mcp/MCPRemoteServerMenu.tsx:284-313`
/// `MCPRemoteServerMenu` local `handleClaudeAIAuth`.
///
/// Deviation (L2, user-authorized OAuth safety gate): the source-owned URL,
/// account, environment, and component-state preparation remain complete;
/// `open_browser` rejects only at its final process-launch outlet.
async fn handle_claude_ai_auth(
    config: &crate::services::mcp::types::ScopedMcpServerConfig,
    on_auth_started: impl FnOnce(String),
) -> anyhow::Result<()> {
    let claude_ai_base_url = crate::constants::oauth::get_oauth_config()?.claude_ai_origin;
    // CC `getOauthAccountInfo()` returns account metadata only while Anthropic
    // auth is the active process authentication mode.
    let account_info = crate::utils::auth::is_anthropic_auth_enabled()
        .then(|| crate::utils::config::load_global_config().oauth_account)
        .flatten();
    let org_uuid = account_info
        .as_ref()
        .and_then(|account| account.organization_uuid.as_deref())
        .filter(|organization_uuid| !organization_uuid.is_empty());

    let auth_url = if let (Some(org_uuid), Transport::ClaudeAiProxy, Some(server_id)) = (
        org_uuid,
        config.transport,
        config
            .id
            .as_deref()
            .filter(|server_id| !server_id.is_empty()),
    ) {
        let server_id = server_id
            .strip_prefix("mcprs")
            .map(|suffix| format!("mcpsrv{suffix}"))
            .unwrap_or_else(|| server_id.to_string());
        let product_surface = std::env::var("CLAUDE_CODE_ENTRYPOINT")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "cli".to_string());
        let mut encoded_product_surface = String::new();
        for byte in product_surface.bytes() {
            match byte {
                b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b'!'
                | b'~'
                | b'*'
                | b'\''
                | b'('
                | b')' => encoded_product_surface.push(byte as char),
                _ => encoded_product_surface.push_str(&format!("%{byte:02X}")),
            }
        }
        format!(
            "{claude_ai_base_url}/api/organizations/{org_uuid}/mcp/start-auth/{server_id}?product_surface={encoded_product_surface}"
        )
    } else {
        format!("{claude_ai_base_url}/settings/connectors")
    };

    on_auth_started(auth_url.clone());
    // CC: logEvent('tengu_claudeai_mcp_auth_started', {}) — joins with analytics.
    let _ = crate::utils::browser::open_browser(&auth_url).await?;
    Ok(())
}

#[component]
pub fn MCPRemoteServerMenu<'a>(
    props: &mut MCPRemoteServerMenuProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let Some(server) = props.server.clone() else {
        return element! { View {} }.into_any();
    };
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let exit_state = use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    let (stdout, _) = hooks.use_output();
    let mut focused_index = hooks.use_state(|| 0usize);
    let actions = menu_actions_for_remote_server(&server);
    let action_count = actions.len().max(1);
    let mut pending_action = hooks.use_state(|| Option::<RemoteServerMenuAction>::None);
    let mut pending_cancel = hooks.use_state(|| false);
    // Maps to: CC `MCPRemoteServerMenu.tsx:81,131,282` — the menu holds
    // `useSetAppState()` for the flows CC writes itself (clear-auth, `:166` and
    // `:443`) and the two manager hooks for the flows the manager owns.
    let runtime_mcp = crate::state::app_state::use_app_state_store(&mut hooks);
    let runtime_mcp = crate::state::app_state_store::McpWriter::new(runtime_mcp);
    let reconnect_mcp_server =
        crate::services::mcp::mcp_connection_manager::use_mcp_reconnect(&mut hooks);
    let toggle_mcp_server =
        crate::services::mcp::mcp_connection_manager::use_mcp_toggle_enabled(&mut hooks);
    let mut is_reconnecting = hooks.use_state(|| false);
    let mut is_authenticating = hooks.use_state(|| false);
    let mut authorization_url = hooks.use_state(|| Option::<String>::None);
    let mut manual_callback_submit =
        hooks.use_state(|| Option::<crate::services::mcp::auth::McpManualCallbackSubmit>::None);
    let mut auth_abort_handle =
        hooks.use_state(|| Option::<crate::services::mcp::auth::McpOAuthAbortHandle>::None);
    let auth_abort_cleanup = hooks
        .use_hook(RemoteOAuthAbortCleanup::default)
        .handle
        .clone();
    let mut callback_url_input = hooks.use_state(String::new);
    let mut is_claude_ai_authenticating = hooks.use_state(|| false);
    let mut claude_ai_auth_url = hooks.use_state(|| Option::<String>::None);
    let mut is_claude_ai_clearing_auth = hooks.use_state(|| false);
    let mut claude_ai_clear_auth_url = hooks.use_state(|| Option::<String>::None);
    let mut claude_ai_clear_auth_browser_opened = hooks.use_state(|| false);
    let mut url_copied = hooks.use_state(|| false);
    let mut auth_error = hooks.use_state(|| Option::<String>::None);
    let mut pending_runtime_result = hooks.use_state(|| Option::<String>::None);
    let mut pending_runtime_cancel = hooks.use_state(|| false);
    let handle_claude_ai_auth_action = hooks.use_async_handler({
        let config = server.config.clone();
        let mut claude_ai_auth_url = claude_ai_auth_url;
        let mut is_claude_ai_authenticating = is_claude_ai_authenticating;
        let mut auth_error = auth_error;
        move |()| {
            let config = config.clone();
            async move {
                auth_error.set(None);
                if let Err(error) = handle_claude_ai_auth(&config, move |url| {
                    claude_ai_auth_url.set(Some(url));
                    is_claude_ai_authenticating.set(true);
                })
                .await
                {
                    auth_error.set(Some(error.to_string()));
                }
            }
        }
    });
    let runtime_action = hooks.use_async_handler({
        let server = server.clone();
        let mut is_reconnecting = is_reconnecting;
        let mut is_authenticating = is_authenticating;
        let mut authorization_url = authorization_url;
        let mut manual_callback_submit = manual_callback_submit;
        let mut auth_abort_handle = auth_abort_handle;
        let auth_abort_cleanup = auth_abort_cleanup.clone();
        let mut callback_url_input = callback_url_input;
        let mut is_claude_ai_authenticating = is_claude_ai_authenticating;
        let mut is_claude_ai_clearing_auth = is_claude_ai_clearing_auth;
        let mut claude_ai_clear_auth_browser_opened = claude_ai_clear_auth_browser_opened;
        let mut auth_error = auth_error;
        let mut pending_runtime_result = pending_runtime_result;
        let mut pending_runtime_cancel = pending_runtime_cancel;
        move |action: RemoteServerMenuAction| {
            let server = server.clone();
            let runtime_mcp = runtime_mcp.clone();
            let reconnect_mcp_server = reconnect_mcp_server.clone();
            let toggle_mcp_server = toggle_mcp_server.clone();
            let auth_abort_cleanup = auth_abort_cleanup.clone();
            async move {
                match action {
                    RemoteServerMenuAction::Reconnect => {
                        // CC `:386` — `const result = await reconnectMcpServer(server.name)`.
                        is_reconnecting.set(true);
                        let client_type = reconnect_mcp_server
                            .call(&server.name)
                            .await
                            .map(|updated| updated.client.status)
                            .unwrap_or_else(|| server.official_client_type());
                        pending_runtime_result.set(Some(
                            handle_reconnect_result(client_type, &server.name).message,
                        ));
                        is_reconnecting.set(false);
                    }
                    RemoteServerMenuAction::Enable | RemoteServerMenuAction::Disable => {
                        // CC `:320-343` — `try { await toggleMcpServer(name); … onCancel() }
                        // catch { onComplete(msg) }`. Only a THROW reports.
                        match toggle_mcp_server.call(&server.name).await {
                            Ok(_) => pending_runtime_cancel.set(true),
                            Err(error) => {
                                let action_name = if server.official_client_type() == McpServerConnectionType::Disabled {
                                    "enable"
                                } else {
                                    "disable"
                                };
                                pending_runtime_result.set(Some(format!(
                                    "Failed to {action_name} MCP server '{}': {error}",
                                    server.name
                                )));
                            }
                        }
                    }
                    RemoteServerMenuAction::ClearAuthentication => {
                        if let Err(error) = crate::services::mcp::auth::revoke_server_tokens(
                            &server.name,
                            &server.config,
                            false,
                        )
                        .await
                        {
                            tracing::debug!(server = %server.name, error = %error, "MCP OAuth clear authentication failed");
                            pending_runtime_result.set(Some(format!(
                                "Failed to clear authentication for {}: {error}",
                                server.name
                            )));
                            return;
                        }
                        crate::services::mcp::client::clear_server_cache(&server.name, None).await;
                        {
                            let base = runtime_mcp
                                .current()
                                .clients
                                .into_iter()
                                .find(|candidate| candidate.client.name == server.name)
                                .unwrap_or_else(|| {
                                    crate::services::mcp::client::McpConnectionDiscovery::pending_with_config(
                                        &server.name,
                                        &server.config,
                                    )
                                    .server
                                });
                            runtime_mcp.apply_server_update(
                                crate::services::mcp::use_manage_mcp_connections::clear_authentication_server_update(&base),
                            );
                        }
                        pending_runtime_result.set(Some(format!(
                            "Authentication cleared for {}.",
                            server.name
                        )));
                    }
                    RemoteServerMenuAction::Authenticate | RemoteServerMenuAction::ReAuthenticate => {
                        is_authenticating.set(true);
                        authorization_url.set(None);
                        manual_callback_submit.set(None);
                        callback_url_input.set(String::new());
                        auth_error.set(None);
                        let abort_handle = crate::services::mcp::auth::McpOAuthAbortHandle::new();
                        auth_abort_handle.set(Some(abort_handle.clone()));
                        if let Ok(mut cleanup_handle) = auth_abort_cleanup.lock() {
                            cleanup_handle.replace(abort_handle.clone());
                        }
                        let authorization_url_state = authorization_url;
                        let on_authorization_url: crate::services::mcp::auth::McpAuthorizationUrlCallback =
                            std::sync::Arc::new(move |url| {
                                let mut state = authorization_url_state;
                                state.set(Some(url));
                            });
                        let manual_callback_submit_state = manual_callback_submit;
                        let on_waiting_for_callback: crate::services::mcp::auth::McpWaitingForCallbackCallback =
                            std::sync::Arc::new(move |submit| {
                                let mut state = manual_callback_submit_state;
                                state.set(Some(submit));
                            });
                        match crate::services::mcp::use_manage_mcp_connections::authenticate_remote_mcp_server_once(
                            &server.name,
                            &server.config,
                            server.is_authenticated.unwrap_or(false),
                            is_effectively_authenticated(&server),
                            Some(on_authorization_url),
                            Some(on_waiting_for_callback),
                            Some(abort_handle.signal()),
                        )
                        .await
                        {
                            Ok((updated, message)) => {
                                runtime_mcp.apply_server_update(updated);
                                pending_runtime_result.set(Some(message));
                            }
                            Err(error) => {
                                if !crate::services::mcp::auth::is_authentication_cancelled_error(&error) {
                                    auth_error.set(Some(error.to_string()));
                                }
                            }
                        }
                        is_authenticating.set(false);
                        manual_callback_submit.set(None);
                        auth_abort_handle.set(None);
                        if let Ok(mut cleanup_handle) = auth_abort_cleanup.lock() {
                            cleanup_handle.take();
                        }
                        callback_url_input.set(String::new());
                    }
                    RemoteServerMenuAction::ClaudeAiAuthenticate => {
                        // CC `handleClaudeAIAuthComplete` (`:133-158`) — `:138`
                        // is `await reconnectMcpServer(server.name)`; the state
                        // write is the manager's, not this menu's.
                        is_claude_ai_authenticating.set(false);
                        is_reconnecting.set(true);
                        let updated = reconnect_mcp_server.call(&server.name).await;
                        let success = updated.as_ref().is_some_and(|updated| {
                            updated.client.status == McpServerConnectionType::Connected
                        });
                        if success {
                            pending_runtime_result.set(Some(format!(
                                "Authentication successful. Connected to {}.",
                                server.name
                            )));
                        } else if updated.as_ref().is_some_and(|updated| {
                            updated.client.status == McpServerConnectionType::NeedsAuth
                        }) {
                            pending_runtime_result.set(Some(
                                "Authentication successful, but server still requires authentication. You may need to manually restart Claude Code."
                                    .to_string(),
                            ));
                        } else {
                            pending_runtime_result.set(Some(
                                "Authentication successful, but server reconnection failed. You may need to manually restart Claude Code for the changes to take effect."
                                    .to_string(),
                            ));
                        }
                        is_reconnecting.set(false);
                    }
                    RemoteServerMenuAction::ClaudeAiClearAuthentication => {
                        crate::services::mcp::client::clear_server_cache(&server.name, None).await;
                        {
                            let base = runtime_mcp
                                .current()
                                .clients
                                .into_iter()
                                .find(|candidate| candidate.client.name == server.name)
                                .unwrap_or_else(|| {
                                    crate::services::mcp::client::McpConnectionDiscovery::pending_with_config(
                                        &server.name,
                                        &server.config,
                                    )
                                    .server
                                });
                            runtime_mcp.apply_server_update(
                                crate::services::mcp::use_manage_mcp_connections::claude_ai_clear_authentication_server_update(&base),
                            );
                        }
                        is_claude_ai_clearing_auth.set(false);
                        claude_ai_clear_auth_browser_opened.set(false);
                        pending_runtime_result.set(Some(format!(
                            "Disconnected from {}.",
                            server.name
                        )));
                    }
                    RemoteServerMenuAction::ViewTools | RemoteServerMenuAction::Back => {}
                }
            }
        }
    });

    // Maps to: CC MCPRemoteServerMenu.tsx:98-101,264-269 copyTimeoutRef.
    // L1 native hook futures are dropped on unmount (source :113-122), so no
    // callback can publish feedback after unmount. A generation invalidates
    // the prior pending timeout when a later clipboard request completes.
    let mut copy_timeout_generation = hooks.use_state(|| 0u64);
    let copy_url_observer = hooks.use_async_handler({
        let stdout = stdout.clone();
        let mut url_copied = url_copied;
        move |copy: tokio::task::JoinHandle<String>| {
            let stdout = stdout.clone();
            async move {
                // Maps to: CC MCPRemoteServerMenu.tsx:261-270. The async
                // handler lifetime is the source unmountedRef guard: disposal
                // drops this continuation and its timeout before either runs.
                let raw = copy.await.expect("clipboard process task panicked");
                if !raw.is_empty() {
                    if let Err(error) = stdout.write_control_sequence_and_wait(raw).await {
                        crate::utils::debug::log_for_debugging(&error.to_string());
                        return;
                    }
                }
                url_copied.set(true);
                let generation = copy_timeout_generation.get().wrapping_add(1);
                copy_timeout_generation.set(generation);
                futures_timer::Delay::new(std::time::Duration::from_millis(2_000)).await;
                if copy_timeout_generation.get() == generation {
                    url_copied.set(false);
                }
            }
        }
    });

    // Start setClipboard independently of the unmount-cancellable observer.
    // Source :262 guards raw/toast/timer only, not native/tmux clipboard work.
    let copy_url_action = Handler::from(move |url: String| {
        let clipboard = stdout.prepare_clipboard(&url);
        let copy = crate::utils::process_runtime::runtime_handle_for_detached_work()
            .expect("clipboard requires the initialized process runtime")
            .spawn(clipboard);
        copy_url_observer(copy);
    });

    let open_browser_action = hooks.use_async_handler({
        let mut auth_error = auth_error;
        move |url: String| async move {
            if let Err(error) = crate::utils::browser::open_browser(&url).await {
                auth_error.set(Some(error.to_string()));
            }
        }
    });

    let runtime_action_for_events = runtime_action.clone();
    let copy_url_action_for_events = copy_url_action.clone();
    let open_browser_action_for_events = open_browser_action.clone();
    hooks.use_propagated_terminal_events({
        let actions = actions.clone();
        let auth_abort_cleanup = auth_abort_cleanup.clone();
        move |event| match event.event() {
            TerminalEvent::Key(KeyEvent { code, kind, .. }) if *kind != KeyEventKind::Release => {
                if is_authenticating.get() {
                    match code {
                        KeyCode::Enter => {
                            let submit = { manual_callback_submit.read().clone() };
                            let value = { callback_url_input.read().trim().to_string() };
                            if let (Some(submit), false) = (submit, value.is_empty()) {
                                submit.submit(value);
                                callback_url_input.set(String::new());
                            }
                            event.stop_propagation();
                        }
                        KeyCode::Char('c') if !url_copied.get() => {
                            if manual_callback_submit.read().is_none() {
                                if let Some(url) = { authorization_url.read().clone() } {
                                    copy_url_action_for_events(url);
                                    event.stop_propagation();
                                }
                            }
                        }
                        KeyCode::Esc => {
                            if let Some(handle) = { auth_abort_handle.read().clone() } {
                                handle.abort();
                            }
                            if let Ok(mut cleanup_handle) = auth_abort_cleanup.lock() {
                                cleanup_handle.take();
                            }
                            is_authenticating.set(false);
                            manual_callback_submit.set(None);
                            auth_abort_handle.set(None);
                            callback_url_input.set(String::new());
                            event.stop_propagation();
                        }
                        _ => {}
                    }
                    return;
                }

                if is_claude_ai_authenticating.get() {
                    match code {
                        KeyCode::Enter => {
                            runtime_action_for_events(RemoteServerMenuAction::ClaudeAiAuthenticate);
                            event.stop_propagation();
                        }
                        KeyCode::Char('c') if !url_copied.get() => {
                            if let Some(url) = { claude_ai_auth_url.read().clone() } {
                                copy_url_action_for_events(url);
                                event.stop_propagation();
                            }
                        }
                        KeyCode::Esc => {
                            is_claude_ai_authenticating.set(false);
                            claude_ai_auth_url.set(None);
                            event.stop_propagation();
                        }
                        _ => {}
                    }
                    return;
                }

                if is_claude_ai_clearing_auth.get() {
                    match code {
                        KeyCode::Enter => {
                            if claude_ai_clear_auth_browser_opened.get() {
                                runtime_action_for_events(
                                    RemoteServerMenuAction::ClaudeAiClearAuthentication,
                                );
                            } else {
                                // Maps to: CC `components/mcp/MCPRemoteServerMenu.tsx:242-256`
                                // clear-auth first-Enter connectors URL/browser branch.
                                match crate::constants::oauth::get_oauth_config() {
                                    Ok(oauth_config) => {
                                        let connectors_url = format!(
                                            "{}/settings/connectors",
                                            oauth_config.claude_ai_origin
                                        );
                                        claude_ai_clear_auth_url.set(Some(connectors_url.clone()));
                                        claude_ai_clear_auth_browser_opened.set(true);
                                        open_browser_action_for_events(connectors_url);
                                    }
                                    Err(error) => auth_error.set(Some(error.to_string())),
                                }
                            }
                            event.stop_propagation();
                        }
                        KeyCode::Char('c') if !url_copied.get() => {
                            if let Some(url) = { claude_ai_clear_auth_url.read().clone() } {
                                copy_url_action_for_events(url);
                                event.stop_propagation();
                            }
                        }
                        KeyCode::Esc => {
                            is_claude_ai_clearing_auth.set(false);
                            claude_ai_clear_auth_url.set(None);
                            claude_ai_clear_auth_browser_opened.set(false);
                            event.stop_propagation();
                        }
                        _ => {}
                    }
                    return;
                }

                match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        let current = focused_index.get();
                        focused_index.set(if current == 0 {
                            action_count - 1
                        } else {
                            current - 1
                        });
                        event.stop_propagation();
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        focused_index.set((focused_index.get() + 1) % action_count);
                        event.stop_propagation();
                    }
                    KeyCode::Enter => {
                        if let Some(action) = actions.get(focused_index.get()).copied() {
                            pending_action.set(Some(action));
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Esc => {
                        pending_cancel.set(true);
                        event.stop_propagation();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    });

    if pending_cancel.get() {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }
    let pending_runtime_result_value = { pending_runtime_result.read().clone() };
    if let Some(result) = pending_runtime_result_value {
        pending_runtime_result.set(None);
        (props.on_complete)(result);
    }
    if pending_runtime_cancel.get() {
        pending_runtime_cancel.set(false);
        (props.on_cancel)(());
    }
    let pending_action_value = { pending_action.read().clone() };
    if let Some(action) = pending_action_value {
        pending_action.set(None);
        match action {
            RemoteServerMenuAction::ViewTools => (props.on_view_tools)(()),
            RemoteServerMenuAction::Reconnect => runtime_action(action),
            RemoteServerMenuAction::Authenticate | RemoteServerMenuAction::ReAuthenticate => {
                runtime_action(action)
            }
            RemoteServerMenuAction::ClearAuthentication => runtime_action(action),
            RemoteServerMenuAction::Enable | RemoteServerMenuAction::Disable => {
                runtime_action(action)
            }
            RemoteServerMenuAction::ClaudeAiAuthenticate => {
                handle_claude_ai_auth_action(());
            }
            RemoteServerMenuAction::ClaudeAiClearAuthentication => {
                auth_error.set(None);
                claude_ai_clear_auth_url.set(None);
                claude_ai_clear_auth_browser_opened.set(false);
                is_claude_ai_clearing_auth.set(true);
            }
            RemoteServerMenuAction::Back => (props.on_cancel)(()),
        }
    }

    if is_authenticating.get() {
        let auth_copy = if server.transport != Transport::ClaudeAiProxy
            && server
                .config
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.get("xaa"))
                .is_some()
        {
            " Authenticating via your identity provider"
        } else {
            " A browser window will open for authentication"
        };
        let authorization_url_value = { authorization_url.read().clone() };
        let url_copied_value = url_copied.get();
        let manual_callback_submit_value = { manual_callback_submit.read().clone() };
        let show_manual_callback_input =
            authorization_url_value.is_some() && manual_callback_submit_value.is_some();
        return element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32, row_gap: 1u32) {
                Text(content: format!("Authenticating with {}…", server.name), color: theme.claude, wrap: TextWrap::NoWrap)
                View(flex_direction: FlexDirection::Row) {
                    Spinner
                    Text(content: auth_copy.to_string(), wrap: TextWrap::NoWrap)
                }
                #(authorization_url_value.map(|url| element! {
                    View(flex_direction: FlexDirection::Column) {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "If your browser doesn't open automatically, copy this URL manually ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                            Text(content: auth_url_copy_hint(url_copied_value).to_string(), color: if url_copied_value { theme.success } else { theme.inactive }, wrap: TextWrap::NoWrap)
                        }
                        Link(url: url)
                    }
                }.into_any()))
                #(if show_manual_callback_input {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                            Text(content: "If the redirect page shows a connection error, paste the URL from your browser's address bar:".to_string(), dim: true, wrap: TextWrap::Wrap)
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "URL > ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                TextInput(
                                    value: callback_url_input.to_string(),
                                    has_focus: true,
                                    on_change: move |value| { let mut callback_url_input = callback_url_input; callback_url_input.set(value); },
                                )
                            }
                        }
                    }.into_any())
                } else { None })
                Text(content: "Return here after authenticating in your browser. Press Esc to go back.".to_string(), dim: true, wrap: TextWrap::NoWrap)
            }
        }.into_any();
    }

    if is_claude_ai_authenticating.get() {
        let claude_ai_auth_url_value = { claude_ai_auth_url.read().clone() };
        let url_copied_value = url_copied.get();
        return element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32, row_gap: 1u32) {
                Text(content: format!("Authenticating with {}…", server.name), color: theme.claude, wrap: TextWrap::NoWrap)
                View(flex_direction: FlexDirection::Row) {
                    Spinner
                    Text(content: " A browser window will open for authentication".to_string(), wrap: TextWrap::NoWrap)
                }
                #(claude_ai_auth_url_value.map(|url| element! {
                    View(flex_direction: FlexDirection::Column) {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "If your browser doesn't open automatically, copy this URL manually ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                            Text(content: auth_url_copy_hint(url_copied_value).to_string(), color: if url_copied_value { theme.success } else { theme.inactive }, wrap: TextWrap::NoWrap)
                        }
                        Link(url: url)
                    }
                }.into_any()))
                View(flex_direction: FlexDirection::Column, margin_left: 3u32) {
                    Text(content: "Press Enter after authenticating in your browser.".to_string(), color: theme.permission, wrap: TextWrap::NoWrap)
                    Text(content: "Esc to back".to_string(), dim: true, italic: true, wrap: TextWrap::NoWrap)
                }
            }
        }.into_any();
    }

    if is_claude_ai_clearing_auth.get() {
        let browser_opened = claude_ai_clear_auth_browser_opened.get();
        let clear_url_value = { claude_ai_clear_auth_url.read().clone() };
        let url_copied_value = url_copied.get();
        return element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32, row_gap: 1u32) {
                Text(content: format!("Clear authentication for {}", server.name), color: theme.claude, wrap: TextWrap::NoWrap)
                #(if browser_opened {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column) {
                            Text(content: "Find the MCP server in the browser and click \"Disconnect\".".to_string(), wrap: TextWrap::Wrap)
                            #(clear_url_value.map(|url| element! {
                                View(flex_direction: FlexDirection::Column) {
                                    View(flex_direction: FlexDirection::Row) {
                                        Text(content: "If your browser didn't open automatically, copy this URL manually ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                                        Text(content: auth_url_copy_hint(url_copied_value).to_string(), color: if url_copied_value { theme.success } else { theme.inactive }, wrap: TextWrap::NoWrap)
                                    }
                                    Link(url: url)
                                }
                            }.into_any()))
                            View(flex_direction: FlexDirection::Column, margin_left: 3u32) {
                                Text(content: "Press Enter when done.".to_string(), color: theme.permission, wrap: TextWrap::NoWrap)
                                Text(content: "Esc to back".to_string(), dim: true, italic: true, wrap: TextWrap::NoWrap)
                            }
                        }
                    }.into_any())
                } else {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column) {
                            Text(content: "This will open claude.ai in the browser. Find the MCP server in the list and click \"Disconnect\".".to_string(), wrap: TextWrap::Wrap)
                            View(flex_direction: FlexDirection::Column, margin_left: 3u32) {
                                Text(content: "Press Enter to open the browser.".to_string(), color: theme.permission, wrap: TextWrap::NoWrap)
                                Text(content: "Esc to back".to_string(), dim: true, italic: true, wrap: TextWrap::NoWrap)
                            }
                        }
                    }.into_any())
                })
            }
        }.into_any();
    }

    if is_reconnecting.get() {
        return element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32, row_gap: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Connecting to ".to_string(), wrap: TextWrap::NoWrap)
                    Text(content: server.name.clone(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: "…".to_string(), wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Row) {
                    Spinner
                    Text(content: " Establishing connection to MCP server".to_string(), wrap: TextWrap::NoWrap)
                }
                Text(content: "This may take a few moments.".to_string(), dim: true, wrap: TextWrap::NoWrap)
            }
        }.into_any();
    }

    let capitalized = capitalize(&server.name);
    let (icon, text) = status_text(server.official_client_type());
    let status_color = match server.official_client_type() {
        McpServerConnectionType::Connected => theme.success,
        McpServerConnectionType::NeedsAuth => theme.warning,
        McpServerConnectionType::Failed => theme.error,
        McpServerConnectionType::Disabled | McpServerConnectionType::Pending => theme.inactive,
    };
    let auth_ok = is_effectively_authenticated(&server);
    let url = server.config.url.clone().unwrap_or_default();
    let options = actions
        .iter()
        .copied()
        .map(option_for_action)
        .collect::<Vec<_>>();
    let auth_error_value = { auth_error.read().clone() };

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(
                flex_direction: FlexDirection::Column,
                border_style: if props.borderless { BorderStyle::None } else { BorderStyle::Round },
                padding_left: 1u32,
                padding_right: 1u32,
            ) {
                View(margin_bottom: 1u32) {
                    Text(content: format!("{capitalized} MCP Server"), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Column) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Status: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: icon.to_string(), color: status_color, wrap: TextWrap::NoWrap)
                        Text(content: format!(" {text}"), wrap: TextWrap::NoWrap)
                    }
                    #(if server.transport == Transport::ClaudeAiProxy { None } else { Some(element! {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "Auth: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                            Text(content: if auth_ok { figures::get().tick.to_string() } else { figures::get().cross.to_string() }, color: if auth_ok { theme.success } else { theme.error }, wrap: TextWrap::NoWrap)
                            Text(content: if auth_ok { " authenticated".to_string() } else { " not authenticated".to_string() }, wrap: TextWrap::NoWrap)
                        }
                    })})
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "URL: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: url, dim: true, wrap: TextWrap::NoWrap)
                    }
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Config location: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: describe_mcp_config_file_path(server.scope), dim: true, wrap: TextWrap::NoWrap)
                    }
                    #(if server.official_client_type() == McpServerConnectionType::Connected {
                        Some(element! {
                            CapabilitiesSection(
                                server_tools_count: server.tools.len(),
                                server_prompts_count: server.prompts_count,
                                server_resources_count: server.resources_count,
                            )
                        }.into_any())
                    } else { None })
                    #(if server.official_client_type() == McpServerConnectionType::Connected && !server.tools.is_empty() {
                        Some(element! {
                            View(flex_direction: FlexDirection::Row) {
                                Text(content: "Tools: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                Text(content: format!("{} tools", server.tools.len()), dim: true, wrap: TextWrap::NoWrap)
                            }
                        }.into_any())
                    } else { None })
                    #(auth_error_value.map(|error| element! {
                        View(margin_top: 1u32) {
                            Text(content: format!("Error: {error}"), color: theme.error, wrap: TextWrap::Wrap)
                        }
                    }.into_any()))
                    View(margin_top: 1u32) {
                        Select(
                            options: options,
                            focused_index: focused_index.get().min(action_count - 1),
                            visible_option_count: 5usize,
                            layout: SelectLayout::Compact,
                            hide_indexes: true,
                        )
                    }
                }
            }
            View(margin_top: 1u32) {
                #(if exit_state.pending {
                    Some(element! {
                        Text(
                            content: format!("Press {} again to exit", exit_state.key_name.unwrap_or("Ctrl-C")),
                            dim: true,
                            italic: true,
                            wrap: TextWrap::NoWrap,
                        )
                    }.into_any())
                } else {
                    Some(element! {
                        ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext { dim: true, italic: true })) {
                            Byline {
                                KeyboardShortcutHint(shortcut: "↑↓".to_string(), action: "navigate".to_string())
                                KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "select".to_string())
                                ConfigurableShortcutHint(
                                    action: "confirm:no".to_string(),
                                    context: "Confirmation".to_string(),
                                    fallback: "Esc".to_string(),
                                    description: "back".to_string(),
                                )
                            }
                        }
                    }.into_any())
                })
            }
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mcp::client::McpConnectionDiscovery;
    use crate::services::mcp::types::{ConfigScope, ScopedMcpServerConfig};
    use crate::state::app_state_store::McpState;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct EnvRestore {
        _env: crate::utils::env_utils::EnvVarGuard,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: &str) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::set(key, value),
            }
        }

        fn unset(key: &'static str) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::unset(key),
            }
        }
    }

    struct GlobalConfigRestore(Option<crate::utils::config::GlobalConfig>);

    impl GlobalConfigRestore {
        fn install(config: crate::utils::config::GlobalConfig) -> Self {
            Self(crate::utils::config::replace_test_global_config(Some(
                config,
            )))
        }
    }

    impl Drop for GlobalConfigRestore {
        fn drop(&mut self) {
            crate::utils::config::replace_test_global_config(self.0.take());
        }
    }

    fn server(
        status: McpServerConnectionType,
        transport: Transport,
        is_authenticated: Option<bool>,
    ) -> ServerInfo {
        ServerInfo {
            name: "remote".to_string(),
            client: mcp_client_state_from_parts(status, Vec::new(), 0, None, None),
            client_type: status,
            scope: ConfigScope::User,
            transport,
            is_authenticated,
            config: ScopedMcpServerConfig {
                name: None,
                scope: ConfigScope::User,
                transport,
                command: None,
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                url: Some("https://example.com/mcp".to_string()),
                headers: std::collections::BTreeMap::new(),
                headers_helper: None,
                oauth: None,
                ide_running_in_windows: None,
                ide_name: None,
                auth_token: None,
                id: None,
                plugin_source: None,
            },
            reconnect_attempt: None,
            max_reconnect_attempts: None,
            tools: Vec::new(),
            prompts_count: 0,
            resources_count: 0,
        }
    }

    #[derive(Default, Props)]
    struct RemoteToggleHarnessProps {
        cancels: Arc<Mutex<usize>>,
        server: Option<ServerInfo>,
        unmount_on_escape: bool,
    }

    #[component]
    fn RemoteToggleHarness(
        props: &RemoteToggleHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let menu_server = props.server.clone().unwrap_or_else(|| {
            server(
                McpServerConnectionType::Connected,
                Transport::Http,
                Some(false),
            )
        });
        let config = menu_server.config.clone();
        let runtime_state = hooks.use_state(move || {
            let mut runtime_server =
                McpConnectionDiscovery::pending_with_config("remote", &config).server;
            runtime_server.client.status = McpServerConnectionType::Connected;
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.mcp = Arc::new(McpState {
                clients: vec![runtime_server],
                ..McpState::default()
            });
            crate::state::store::AppStore::new(initial, None)
        });
        let mut mounted = hooks.use_state(|| true);
        let unmount_on_escape = props.unmount_on_escape;
        hooks.use_terminal_events(move |event| {
            if unmount_on_escape
                && matches!(
                    event,
                    TerminalEvent::Key(KeyEvent {
                        code: KeyCode::Esc,
                        ..
                    })
                )
            {
                mounted.set(false);
            }
        });
        if !mounted.get() {
            return element! { Text(content: "mcp-parent-after-unmount") }.into_any();
        }
        let cancels = Arc::clone(&props.cancels);
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(runtime_state.read().clone()),
                    children: crate::state::app_state::ProviderChildren::new({
                        let store = runtime_state.read().clone();
                        move || {
                        let cancels = Arc::clone(&cancels);
                        let menu_server = menu_server.clone();
                        element! {
                            crate::services::mcp::mcp_connection_manager::McpConnectionManager(
                                app_store: Some(store.clone()),
                            ) {
                                MCPRemoteServerMenu(
                                    server: Some(menu_server),
                                    on_cancel: move |_| *cancels.lock().expect("cancel mutex") += 1,
                                )
                            }
                        }
                        .into_any()
                    }}),
                )
            }
        }
        .into_any()
    }

    #[test]
    fn remote_menu_disable_dispatches_to_mcp_connection_service() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-remote-menu-toggle-{}",
            uuid::Uuid::new_v4()
        ));
        let project_dir = temp_dir.join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let _config = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", &temp_dir);
        let _session_write =
            crate::utils::env_utils::EnvVarGuard::set("SESSION_WRITE_ENABLED", "1");
        let _write = crate::utils::env_utils::EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        std::env::set_current_dir(&project_dir).unwrap();

        let cancels = Arc::new(Mutex::new(0usize));
        let cancels_for_app = Arc::clone(&cancels);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut app = element! { RemoteToggleHarness(cancels: cancels_for_app) };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::iter(vec![
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down)),
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down)),
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
                    ]))
                    .with_size(120, 24),
                ),
            );
            for _ in 0..10 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(*cancels.lock().expect("cancel mutex"), 1);
        assert!(crate::services::mcp::config::is_mcp_server_disabled(
            "remote"
        ));

        std::env::set_current_dir(previous_cwd).unwrap();
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn remote_menu_renders_official_round_box_status_actions_and_footer() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        crate::services::mcp::mcp_connection_manager::McpConnectionManager() {
                            MCPRemoteServerMenu(server: Some(server(
                                McpServerConnectionType::Connected,
                                Transport::Http,
                                Some(true),
                            )))
                        }
                    }.into_any()),
                )
            }
        }
        .render(Some(100));
        let text = canvas.to_string();

        assert!(
            text.lines().any(|line| line.starts_with('╭')),
            "canvas=\n{text}"
        );
        assert!(text.contains("Remote MCP Server"), "canvas=\n{text}");
        assert!(
            text.contains(&format!("Status: {} connected", figures::get().tick)),
            "canvas=\n{text}"
        );
        assert!(text.contains("Auth: "), "canvas=\n{text}");
        assert!(text.contains("authenticated"), "canvas=\n{text}");
        assert!(
            text.contains("URL: https://example.com/mcp"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Re-authenticate"), "canvas=\n{text}");
        assert!(text.contains("Clear authentication"), "canvas=\n{text}");
        assert!(text.contains("Reconnect"), "canvas=\n{text}");
        assert!(text.contains("Disable"), "canvas=\n{text}");
        assert_eq!(text.matches("↑↓ to navigate").count(), 1, "canvas=\n{text}");
        assert!(
            text.contains("Enter to select · Esc to back"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn remote_auth_url_copy_hint_matches_official_states() {
        assert_eq!(auth_url_copy_hint(false), "(c copy)");
        assert_eq!(auth_url_copy_hint(true), "(Copied!)");
    }

    #[test]
    fn handle_claude_ai_auth_matches_official_url_selection_and_closed_outlet() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _oauth_env = [
            EnvRestore::unset("USE_LOCAL_OAUTH"),
            EnvRestore::unset("USE_STAGING_OAUTH"),
            EnvRestore::unset("CLAUDE_CODE_CUSTOM_OAUTH_URL"),
            EnvRestore::unset("CLAUDE_CODE_SIMPLE"),
            EnvRestore::set("ANTHROPIC_UNIX_SOCKET", "/tmp/cometix-test-auth.sock"),
            EnvRestore::set("CLAUDE_CODE_OAUTH_TOKEN", "test-oauth-token"),
        ];
        let _entrypoint = EnvRestore::set("CLAUDE_CODE_ENTRYPOINT", "cli test");
        let mut account = crate::utils::config::AccountInfo::default();
        account.organization_uuid = Some("org_123".to_string());
        let _global_config = GlobalConfigRestore::install(crate::utils::config::GlobalConfig {
            oauth_account: Some(account),
            ..crate::utils::config::GlobalConfig::default()
        });

        let mut direct_config = server(
            McpServerConnectionType::NeedsAuth,
            Transport::ClaudeAiProxy,
            None,
        )
        .config;
        direct_config.id = Some("mcprs_abc".to_string());
        let direct_url = Arc::new(Mutex::new(None::<String>));
        let direct_url_for_callback = Arc::clone(&direct_url);
        let direct_error =
            futures::executor::block_on(handle_claude_ai_auth(&direct_config, move |url| {
                *direct_url_for_callback.lock().expect("direct URL mutex") = Some(url);
            }))
            .expect_err("the prepared OAuth browser outlet must remain default-closed");
        assert!(
            direct_error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        assert_eq!(
            direct_url.lock().expect("direct URL mutex").as_deref(),
            Some(
                "https://claude.ai/api/organizations/org_123/mcp/start-auth/mcpsrv_abc?product_surface=cli%20test"
            )
        );

        direct_config.id = None;
        let fallback_url = Arc::new(Mutex::new(None::<String>));
        let fallback_url_for_callback = Arc::clone(&fallback_url);
        let fallback_error =
            futures::executor::block_on(handle_claude_ai_auth(&direct_config, move |url| {
                *fallback_url_for_callback
                    .lock()
                    .expect("fallback URL mutex") = Some(url);
            }))
            .expect_err("the fallback browser outlet must remain default-closed");
        assert!(
            fallback_error
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
        );
        assert_eq!(
            fallback_url.lock().expect("fallback URL mutex").as_deref(),
            Some("https://claude.ai/settings/connectors")
        );
    }

    #[test]
    fn remote_oauth_cleanup_aborts_on_drop_like_official_unmount() {
        let handle = crate::services::mcp::auth::McpOAuthAbortHandle::new();
        let signal = handle.signal();
        let cleanup = RemoteOAuthAbortCleanup::default();
        *cleanup.handle.lock().expect("cleanup mutex") = Some(handle);
        drop(cleanup);
        assert!(signal.is_aborted());
    }

    #[test]
    fn remote_menu_actions_match_official_auth_and_disabled_branches() {
        assert_eq!(
            menu_actions_for_remote_server(&server(
                McpServerConnectionType::Disabled,
                Transport::Http,
                None
            )),
            vec![
                RemoteServerMenuAction::Enable,
                RemoteServerMenuAction::Authenticate
            ]
        );
        assert_eq!(
            menu_actions_for_remote_server(&server(
                McpServerConnectionType::NeedsAuth,
                Transport::Http,
                None
            )),
            vec![
                RemoteServerMenuAction::Authenticate,
                RemoteServerMenuAction::Disable
            ]
        );
        assert_eq!(
            menu_actions_for_remote_server(&server(
                McpServerConnectionType::Connected,
                Transport::Http,
                Some(true)
            )),
            vec![
                RemoteServerMenuAction::ReAuthenticate,
                RemoteServerMenuAction::ClearAuthentication,
                RemoteServerMenuAction::Reconnect,
                RemoteServerMenuAction::Disable,
            ]
        );
    }

    #[test]
    fn claudeai_proxy_menu_uses_claudeai_auth_options() {
        assert_eq!(
            menu_actions_for_remote_server(&server(
                McpServerConnectionType::Pending,
                Transport::ClaudeAiProxy,
                None
            )),
            vec![
                RemoteServerMenuAction::ClaudeAiAuthenticate,
                RemoteServerMenuAction::Reconnect,
                RemoteServerMenuAction::Disable,
            ]
        );
    }

    /// CC :257-269 ignores c while copied, then restores the hint after 2000ms.
    /// The browser outlet is the existing closed OAuth boundary; only the
    /// canonical SSH OSC clipboard path runs, without native clipboard writes.
    #[tokio::test]
    async fn remote_clipboard_matches_official_feedback_and_copy_timeout() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _ssh = EnvRestore::set("SSH_CONNECTION", "fixture");
        let _tmux = EnvRestore::unset("TMUX");
        let _oauth = EnvRestore::unset("CLAUDE_CODE_CUSTOM_OAUTH_URL");
        let (sender, receiver) = async_channel::unbounded();
        let mut app = element! {
            ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                RemoteToggleHarness(server: Some(server(McpServerConnectionType::NeedsAuth, Transport::ClaudeAiProxy, None)), cancels: Arc::new(Mutex::new(0)))
            }
        };
        let mut app = element! {
            ContextProvider(value: Context::owned(iocraft::Clipboard::new(std::sync::Arc::new(crate::utils::exec_file_no_throw::ExecFileClipboardBackend)))) {
                #(app)
            }
        };
        let mut output = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(receiver).with_size(120, 24),
        ));
        let mut frames = Vec::new();
        let collect = async {
            while let Some(frame) = output.next().await {
                frames.push(frame.to_string());
            }
        };
        let drive = async move {
            for (delay, code) in [
                (40, KeyCode::Enter),
                (80, KeyCode::Char('c')),
                (1_000, KeyCode::Char('c')),
            ] {
                futures_timer::Delay::new(Duration::from_millis(delay)).await;
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)))
                    .await
                    .unwrap();
            }
            // Second c is ignored: the first copy's deadline remains effective.
            futures_timer::Delay::new(Duration::from_millis(1_200)).await;
        };
        crate::utils::race(collect, drive).await;
        assert!(
            frames.iter().any(|frame| frame.contains("(Copied!)")),
            "{frames:?}"
        );
        let last = frames.last().unwrap();
        assert!(last.contains("(c copy)"), "{frames:?}");
        assert!(!last.contains("(Copied!)"), "{frames:?}");
    }

    /// CC :111-122,261-270 keeps setClipboard running but suppresses raw,
    /// copied feedback and the new timeout after a parent removes the menu.
    #[cfg(unix)]
    #[tokio::test]
    async fn remote_clipboard_matches_official_parent_unmount_guard() {
        use std::os::unix::fs::PermissionsExt;
        crate::utils::process_runtime::initialize_test_process_runtime();
        let directory =
            std::env::temp_dir().join(format!("cometix-mcp-clipboard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let tool = directory.join("tmux");
        std::fs::write(&tool, "#!/bin/sh\n/bin/cat > \"$CLIPBOARD_FIXTURE_DIR/input\"\n/bin/sleep 0.25\n: > \"$CLIPBOARD_FIXTURE_DIR/finished\"\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _path = crate::utils::env_utils::EnvVarGuard::set("PATH", &directory);
        let _directory =
            crate::utils::env_utils::EnvVarGuard::set("CLIPBOARD_FIXTURE_DIR", &directory);
        let _ssh = EnvRestore::set("SSH_CONNECTION", "fixture");
        let _tmux = EnvRestore::set("TMUX", "fixture");
        let _oauth = EnvRestore::unset("CLAUDE_CODE_CUSTOM_OAUTH_URL");
        let (sender, receiver) = async_channel::unbounded();
        let mut app = element! {
            ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                RemoteToggleHarness(server: Some(server(McpServerConnectionType::NeedsAuth, Transport::ClaudeAiProxy, None)), cancels: Arc::new(Mutex::new(0)), unmount_on_escape: true)
            }
        };
        let mut app = element! {
            ContextProvider(value: Context::owned(iocraft::Clipboard::new(std::sync::Arc::new(crate::utils::exec_file_no_throw::ExecFileClipboardBackend)))) {
                #(app)
            }
        };
        let mut output = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(receiver).with_size(120, 24),
        ));
        let mut frames = Vec::new();
        let collect = async {
            while let Some(frame) = output.next().await {
                frames.push(frame.to_string());
            }
        };
        let drive = async move {
            for (delay, code) in [
                (40, KeyCode::Enter),
                (80, KeyCode::Char('c')),
                (60, KeyCode::Esc),
            ] {
                futures_timer::Delay::new(Duration::from_millis(delay)).await;
                sender
                    .send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)))
                    .await
                    .unwrap();
            }
            futures_timer::Delay::new(Duration::from_millis(450)).await;
        };
        crate::utils::race(collect, drive).await;
        assert!(directory.join("finished").exists());
        assert!(
            std::fs::read_to_string(directory.join("input"))
                .unwrap()
                .contains("/settings/connectors")
        );
        assert!(
            !frames.iter().any(|frame| frame.contains("(Copied!)")),
            "{frames:?}"
        );
        assert!(
            frames.last().unwrap().contains("mcp-parent-after-unmount"),
            "{frames:?}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
