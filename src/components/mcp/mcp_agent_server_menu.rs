//! Maps to: CC `components/mcp/MCPAgentServerMenu.tsx`.
//!
//! Agent-only metadata is rendered here; OAuth protocol execution is delegated
//! to `services/mcp/auth.rs` like the official component delegates to
//! `performMCPOAuthFlow`.

use super::types::AgentMcpServerInfo;
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use crate::components::spinner::Spinner;
use crate::constants::figures;
use crate::services::mcp::types::{ConfigScope, ScopedMcpServerConfig, Transport};
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentServerMenuAction {
    Authenticate,
    Back,
}

#[derive(Default, Props)]
pub struct MCPAgentServerMenuProps<'a> {
    pub agent_server: Option<AgentMcpServerInfo>,
    pub on_cancel: HandlerMut<'a, ()>,
    pub on_complete: Handler<String>,
}

/// Maps to: CC `MCPAgentServerMenu.tsx` `useEffect(() => () =>
/// authAbortControllerRef.current?.abort(), [])` cleanup.
#[derive(Default)]
struct AgentOAuthAbortCleanup {
    handle:
        std::sync::Arc<std::sync::Mutex<Option<crate::services::mcp::auth::McpOAuthAbortHandle>>>,
}

impl Hook for AgentOAuthAbortCleanup {}

impl Drop for AgentOAuthAbortCleanup {
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

pub fn menu_actions_for_agent_server(server: &AgentMcpServerInfo) -> Vec<AgentServerMenuAction> {
    let mut actions = Vec::new();
    if server.needs_auth {
        actions.push(AgentServerMenuAction::Authenticate);
    }
    actions.push(AgentServerMenuAction::Back);
    actions
}

/// Maps to: CC `components/mcp/MCPAgentServerMenu.tsx#handleAuthenticate`
/// temporary config `{ type: agentServer.transport, url: agentServer.url }`.
pub fn agent_server_oauth_config(server: &AgentMcpServerInfo) -> Option<ScopedMcpServerConfig> {
    if !server.needs_auth || !matches!(server.transport, Transport::Http | Transport::Sse) {
        return None;
    }
    let url = server.url.as_ref().filter(|url| !url.trim().is_empty())?;
    Some(ScopedMcpServerConfig {
        name: None,
        // CC's temporary config has no persisted scope; Dynamic keeps this
        // service-only shape out of user/project config lifecycles.
        scope: ConfigScope::Dynamic,
        transport: server.transport,
        command: None,
        args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        url: Some(url.clone()),
        headers: std::collections::BTreeMap::new(),
        headers_helper: None,
        oauth: None,
        ide_running_in_windows: None,
        ide_name: None,
        auth_token: None,
        id: None,
        plugin_source: None,
    })
}

/// Maps to: CC `MCPAgentServerMenu.tsx#handleAuthenticate` success copy.
pub fn agent_authentication_success_message(server_name: &str) -> String {
    format!(
        "Authentication successful for {server_name}. The server will connect when the agent runs."
    )
}

fn option_for_action(
    action: AgentServerMenuAction,
    server: &AgentMcpServerInfo,
) -> SelectOptionData {
    let label = match action {
        AgentServerMenuAction::Authenticate => {
            if server.is_authenticated {
                "Re-authenticate"
            } else {
                "Authenticate"
            }
        }
        AgentServerMenuAction::Back => "Back",
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

#[component]
pub fn MCPAgentServerMenu<'a>(
    props: &mut MCPAgentServerMenuProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let Some(agent_server) = props.agent_server.clone() else {
        return element! { View {} }.into_any();
    };
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let mut focused_index = hooks.use_state(|| 0usize);
    let actions = menu_actions_for_agent_server(&agent_server);
    let action_count = actions.len().max(1);
    let mut pending_action = hooks.use_state(|| Option::<AgentServerMenuAction>::None);
    let mut pending_cancel = hooks.use_state(|| false);
    let mut is_authenticating = hooks.use_state(|| false);
    let mut auth_error = hooks.use_state(|| Option::<String>::None);
    let mut authorization_url = hooks.use_state(|| Option::<String>::None);
    let mut auth_abort_handle =
        hooks.use_state(|| Option::<crate::services::mcp::auth::McpOAuthAbortHandle>::None);
    let auth_abort_cleanup = hooks
        .use_hook(AgentOAuthAbortCleanup::default)
        .handle
        .clone();
    let mut pending_auth_result = hooks.use_state(|| Option::<String>::None);
    let auth_action = hooks.use_async_handler({
        let agent_server = agent_server.clone();
        let mut is_authenticating = is_authenticating;
        let mut auth_error = auth_error;
        let mut authorization_url = authorization_url;
        let mut auth_abort_handle = auth_abort_handle;
        let auth_abort_cleanup = auth_abort_cleanup.clone();
        let mut pending_auth_result = pending_auth_result;
        move |action: AgentServerMenuAction| {
            let agent_server = agent_server.clone();
            let auth_abort_cleanup = auth_abort_cleanup.clone();
            async move {
                if action != AgentServerMenuAction::Authenticate {
                    return;
                }
                let Some(config) = agent_server_oauth_config(&agent_server) else {
                    return;
                };
                is_authenticating.set(true);
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
                match crate::services::mcp::auth::perform_mcp_oauth_flow(
                    &agent_server.name,
                    &config,
                    crate::services::mcp::auth::McpOAuthFlowOptions {
                        skip_browser_open: false,
                        on_authorization_url: Some(on_authorization_url),
                        on_waiting_for_callback: None,
                        abort_signal: Some(abort_handle.signal()),
                    },
                )
                .await
                {
                    Ok(_) => pending_auth_result.set(Some(agent_authentication_success_message(
                        &agent_server.name,
                    ))),
                    Err(error) => {
                        if !crate::services::mcp::auth::is_authentication_cancelled_error(&error) {
                            auth_error.set(Some(error.to_string()));
                        }
                    }
                }
                is_authenticating.set(false);
                auth_abort_handle.set(None);
                if let Ok(mut cleanup_handle) = auth_abort_cleanup.lock() {
                    cleanup_handle.take();
                }
            }
        }
    });

    hooks.use_propagated_terminal_events({
        let actions = actions.clone();
        let auth_abort_cleanup = auth_abort_cleanup.clone();
        move |event| match event.event() {
            TerminalEvent::Key(KeyEvent { code, kind, .. }) if *kind != KeyEventKind::Release => {
                if is_authenticating.get() {
                    if *code == KeyCode::Esc {
                        if let Some(handle) = { auth_abort_handle.read().clone() } {
                            handle.abort();
                        }
                        if let Ok(mut cleanup_handle) = auth_abort_cleanup.lock() {
                            cleanup_handle.take();
                        }
                        is_authenticating.set(false);
                        authorization_url.set(None);
                        auth_abort_handle.set(None);
                        event.stop_propagation();
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
    let pending_auth_result_value = { pending_auth_result.read().clone() };
    if let Some(result) = pending_auth_result_value {
        pending_auth_result.set(None);
        (props.on_complete)(result);
    }
    let pending_action_value = { pending_action.read().clone() };
    if let Some(action) = pending_action_value {
        pending_action.set(None);
        match action {
            AgentServerMenuAction::Authenticate => auth_action(action),
            AgentServerMenuAction::Back => (props.on_cancel)(()),
        }
    }

    let mut pending_dialog_cancel = hooks.use_state(|| false);
    if pending_dialog_cancel.get() {
        pending_dialog_cancel.set(false);
        (props.on_cancel)(());
    }

    if is_authenticating.get() {
        let authorization_url_value = { authorization_url.read().clone() };
        return element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32) {
                Text(content: format!("Authenticating with {}…", agent_server.name), color: theme.claude, wrap: TextWrap::NoWrap)
                View(flex_direction: FlexDirection::Row) {
                    Spinner
                    Text(content: " A browser window will open for authentication".to_string(), wrap: TextWrap::NoWrap)
                }
                #(authorization_url_value.map(|url| element! {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: "If your browser doesn't open automatically, copy this URL manually:".to_string(), dim: true, wrap: TextWrap::NoWrap)
                        Link(url: url)
                    }
                }.into_any()))
                View(margin_left: 3u32) {
                    Text(content: "Return here after authenticating in your browser. Press Esc to go back.".to_string(), dim: true, wrap: TextWrap::NoWrap)
                }
            }
        }.into_any();
    }

    let capitalized = capitalize(&agent_server.name);
    let options = actions
        .iter()
        .copied()
        .map(|action| option_for_action(action, &agent_server))
        .collect::<Vec<_>>();

    element! {
        Dialog(
            title: format!("{capitalized} MCP Server"),
            subtitle: Some("agent-only".to_string()),
            input_guide_children: vec![element! {
                Byline {
                    KeyboardShortcutHint(shortcut: "↑↓".to_string(), action: "navigate".to_string())
                    KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
                    ConfigurableShortcutHint(
                        action: "confirm:no".to_string(),
                        context: "Confirmation".to_string(),
                        fallback: "Esc".to_string(),
                        description: "go back".to_string(),
                    )
                }
            }.into_any()],
            on_cancel: move |_| pending_dialog_cancel.set(true),
        ) {
            View(flex_direction: FlexDirection::Column) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Type: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: agent_server.transport.as_str().to_string(), dim: true, wrap: TextWrap::NoWrap)
                }
                #(agent_server.url.as_ref().map(|url| element! {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "URL: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: url.clone(), dim: true, wrap: TextWrap::NoWrap)
                    }
                }))
                #(agent_server.command.as_ref().map(|command| element! {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Command: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: command.clone(), dim: true, wrap: TextWrap::NoWrap)
                    }
                }))
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Used by: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: agent_server.source_agents.join(", "), dim: true, wrap: TextWrap::NoWrap)
                }
                View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                    Text(content: "Status: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: figures::get().radio_off.to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    Text(content: " not connected (agent-only)".to_string(), wrap: TextWrap::NoWrap)
                }
                #(if agent_server.needs_auth {
                    Some(element! {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "Auth: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                            Text(
                                content: if agent_server.is_authenticated { figures::get().tick.to_string() } else { figures::get().triangle_up_outline.to_string() },
                                color: if agent_server.is_authenticated { theme.success } else { theme.warning },
                                wrap: TextWrap::NoWrap,
                            )
                            Text(
                                content: if agent_server.is_authenticated { " authenticated".to_string() } else { " may need authentication".to_string() },
                                wrap: TextWrap::NoWrap,
                            )
                        }
                    }.into_any())
                } else { None })
                Text(content: "This server connects only when running the agent.".to_string(), dim: true, wrap: TextWrap::Wrap)
                #({ auth_error.read().clone() }.map(|error| element! {
                    View {
                        Text(content: format!("Error: {error}"), color: theme.error, wrap: TextWrap::Wrap)
                    }
                }))
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
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mcp::types::Transport;

    fn auth_server() -> AgentMcpServerInfo {
        AgentMcpServerInfo {
            name: "docs".to_string(),
            transport: Transport::Http,
            url: Some("https://example.com".to_string()),
            command: None,
            source_agents: vec!["researcher".to_string()],
            needs_auth: true,
            is_authenticated: false,
        }
    }

    #[test]
    fn agent_menu_actions_match_official_auth_branch() {
        let server = auth_server();
        assert_eq!(
            menu_actions_for_agent_server(&server),
            vec![
                AgentServerMenuAction::Authenticate,
                AgentServerMenuAction::Back
            ]
        );
    }

    #[test]
    fn agent_oauth_cleanup_aborts_on_drop_like_official_unmount() {
        let handle = crate::services::mcp::auth::McpOAuthAbortHandle::new();
        let signal = handle.signal();
        let cleanup = AgentOAuthAbortCleanup::default();
        *cleanup.handle.lock().expect("cleanup mutex") = Some(handle);
        drop(cleanup);
        assert!(signal.is_aborted());
    }

    #[test]
    fn agent_oauth_temp_config_matches_official_shape() {
        let config = agent_server_oauth_config(&auth_server()).unwrap();
        assert_eq!(config.scope, ConfigScope::Dynamic);
        assert_eq!(config.transport, Transport::Http);
        assert_eq!(config.url.as_deref(), Some("https://example.com"));
        assert!(config.command.is_none());
        assert!(config.headers.is_empty());
        assert!(config.oauth.is_none());
    }

    #[test]
    fn agent_auth_success_copy_matches_official_component() {
        assert_eq!(
            agent_authentication_success_message("docs"),
            "Authentication successful for docs. The server will connect when the agent runs."
        );
    }
}
