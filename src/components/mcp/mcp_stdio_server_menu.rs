//! Maps to: CC `components/mcp/MCPStdioServerMenu.tsx`.
//!
//! Selection callbacks dispatch to the service-owned MCP connection manager;
//! transport lifecycle and settings writes remain in `services/mcp/*`.

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
use crate::services::mcp::types::McpServerConnectionType;
use crate::services::mcp::utils::describe_mcp_config_file_path;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdioServerMenuAction {
    ViewTools,
    Reconnect,
    ToggleEnabled,
    Back,
}

#[derive(Default, Props)]
pub struct MCPStdioServerMenuProps<'a> {
    pub server: Option<ServerInfo>,
    pub on_view_tools: HandlerMut<'a, ()>,
    pub on_cancel: HandlerMut<'a, ()>,
    pub on_complete: Handler<String>,
    pub borderless: bool,
}

fn capitalize(input: &str) -> String {
    let mut chars = input.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn menu_actions_for_stdio_server(server: &ServerInfo) -> Vec<StdioServerMenuAction> {
    let mut actions = Vec::new();
    if server.official_client_type() != McpServerConnectionType::Disabled
        && !server.tools.is_empty()
    {
        actions.push(StdioServerMenuAction::ViewTools);
    }
    if server.official_client_type() != McpServerConnectionType::Disabled {
        actions.push(StdioServerMenuAction::Reconnect);
    }
    actions.push(StdioServerMenuAction::ToggleEnabled);
    if actions.is_empty() {
        actions.push(StdioServerMenuAction::Back);
    }
    actions
}

fn option_for_action(action: StdioServerMenuAction, server: &ServerInfo) -> SelectOptionData {
    let label = match action {
        StdioServerMenuAction::ViewTools => "View tools".to_string(),
        StdioServerMenuAction::Reconnect => "Reconnect".to_string(),
        StdioServerMenuAction::ToggleEnabled => {
            if server.official_client_type() == McpServerConnectionType::Disabled {
                "Enable".to_string()
            } else {
                "Disable".to_string()
            }
        }
        StdioServerMenuAction::Back => "Back".to_string(),
    };
    SelectOptionData {
        value: format!("{:?}", action),
        label,
        description: None,
        dim_description: false,
        disabled: false,
        input: None,
    }
}

fn status_parts(status: McpServerConnectionType) -> (&'static str, &'static str) {
    let figures = figures::get();
    match status {
        McpServerConnectionType::Disabled => (figures.radio_off, "disabled"),
        McpServerConnectionType::Connected => (figures.tick, "connected"),
        McpServerConnectionType::Pending => (figures.radio_off, "connecting…"),
        McpServerConnectionType::NeedsAuth | McpServerConnectionType::Failed => {
            (figures.cross, "failed")
        }
    }
}

#[component]
pub fn MCPStdioServerMenu<'a>(
    props: &mut MCPStdioServerMenuProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let Some(server) = props.server.clone() else {
        return element! { View {} }.into_any();
    };
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let exit_state = use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    let mut focused_index = hooks.use_state(|| 0usize);
    let actions = menu_actions_for_stdio_server(&server);
    let action_count = actions.len().max(1);
    let mut pending_action = hooks.use_state(|| Option::<StdioServerMenuAction>::None);
    let mut pending_cancel = hooks.use_state(|| false);
    // Maps to: CC `MCPStdioServerMenu.tsx:53-54` — the two manager hooks. The
    // connect/disconnect state writes belong to `useManageMCPConnections`; this
    // menu only invokes them and reports the outcome (`:57-71`).
    let reconnect_mcp_server =
        crate::services::mcp::mcp_connection_manager::use_mcp_reconnect(&mut hooks);
    let toggle_mcp_server =
        crate::services::mcp::mcp_connection_manager::use_mcp_toggle_enabled(&mut hooks);
    let mut is_reconnecting = hooks.use_state(|| false);
    let mut pending_runtime_result = hooks.use_state(|| Option::<String>::None);
    let mut pending_runtime_cancel = hooks.use_state(|| false);
    let runtime_action = hooks.use_async_handler({
        let server = server.clone();
        let mut is_reconnecting = is_reconnecting;
        let mut pending_runtime_result = pending_runtime_result;
        let mut pending_runtime_cancel = pending_runtime_cancel;
        move |action: StdioServerMenuAction| {
            let server = server.clone();
            let reconnect_mcp_server = reconnect_mcp_server.clone();
            let toggle_mcp_server = toggle_mcp_server.clone();
            async move {
                match action {
                    StdioServerMenuAction::Reconnect => {
                        // CC `:201` — `const result = await reconnectMcpServer(server.name)`.
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
                    StdioServerMenuAction::ToggleEnabled => {
                        // CC `:57-71` — `try { await toggleMcpServer(name); onCancel() }
                        // catch { onComplete(msg) }`. Only a THROW reports; an
                        // enable that succeeds but cannot connect is not one.
                        match toggle_mcp_server.call(&server.name).await {
                            Ok(_) => pending_runtime_cancel.set(true),
                            Err(error) => {
                                let action_name = if server.official_client_type()
                                    == McpServerConnectionType::Disabled
                                {
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
                    StdioServerMenuAction::ViewTools | StdioServerMenuAction::Back => {}
                }
            }
        }
    });

    hooks.use_propagated_terminal_events({
        let actions = actions.clone();
        move |event| match event.event() {
            TerminalEvent::Key(KeyEvent { code, kind, .. }) if *kind != KeyEventKind::Release => {
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
            StdioServerMenuAction::ViewTools => (props.on_view_tools)(()),
            StdioServerMenuAction::Reconnect | StdioServerMenuAction::ToggleEnabled => {
                runtime_action(action)
            }
            StdioServerMenuAction::Back => (props.on_cancel)(()),
        }
    }

    if is_reconnecting.get() {
        return element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Reconnecting to ".to_string(), wrap: TextWrap::NoWrap)
                    Text(content: server.name.clone(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Row) {
                    Spinner
                    Text(content: " Restarting MCP server process".to_string(), wrap: TextWrap::NoWrap)
                }
                Text(content: "This may take a few moments.".to_string(), dim: true, wrap: TextWrap::NoWrap)
            }
        }.into_any();
    }

    let capitalized = capitalize(&server.name);
    let (status_icon, status_text) = status_parts(server.official_client_type());
    let status_color = match server.official_client_type() {
        McpServerConnectionType::Connected => theme.success,
        McpServerConnectionType::Disabled | McpServerConnectionType::Pending => theme.inactive,
        McpServerConnectionType::NeedsAuth | McpServerConnectionType::Failed => theme.error,
    };
    let options = actions
        .iter()
        .copied()
        .map(|action| option_for_action(action, &server))
        .collect::<Vec<_>>();
    let command = server.config.command.clone().unwrap_or_default();

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
                        Text(content: status_icon.to_string(), color: status_color, wrap: TextWrap::NoWrap)
                        Text(content: format!(" {status_text}"), wrap: TextWrap::NoWrap)
                    }
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Command: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: command, dim: true, wrap: TextWrap::NoWrap)
                    }
                    #(if server.config.args.is_empty() { None } else { Some(element! {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "Args: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                            Text(content: server.config.args.join(" "), dim: true, wrap: TextWrap::NoWrap)
                        }
                    })})
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
    use crate::services::mcp::types::{ConfigScope, ScopedMcpServerConfig, Transport};
    use crate::state::app_state_store::McpState;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn server(status: McpServerConnectionType) -> ServerInfo {
        ServerInfo {
            name: "docs".to_string(),
            client: mcp_client_state_from_parts(status, Vec::new(), 0, None, None),
            client_type: status,
            scope: ConfigScope::Project,
            transport: Transport::Stdio,
            is_authenticated: None,
            config: ScopedMcpServerConfig {
                name: None,
                scope: ConfigScope::Project,
                transport: Transport::Stdio,
                command: Some("node".to_string()),
                args: vec!["server.js".to_string()],
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
            },
            reconnect_attempt: None,
            max_reconnect_attempts: None,
            tools: Vec::new(),
            prompts_count: 0,
            resources_count: 0,
        }
    }

    #[derive(Default, Props)]
    struct StdioToggleHarnessProps {
        cancels: Arc<Mutex<usize>>,
    }

    #[component]
    fn StdioToggleHarness(
        props: &StdioToggleHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let menu_server = server(McpServerConnectionType::Connected);
        let config = menu_server.config.clone();
        let runtime_state = hooks.use_state(move || {
            let mut runtime_server =
                McpConnectionDiscovery::pending_with_config("docs", &config).server;
            runtime_server.client.status = McpServerConnectionType::Connected;
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.mcp = Arc::new(McpState {
                clients: vec![runtime_server],
                ..McpState::default()
            });
            crate::state::store::AppStore::new(initial, None)
        });
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
                            // The menu reads `useMcpReconnect`/`useMcpToggleEnabled`,
                            // which throw outside the manager — same as the source.
                            crate::services::mcp::mcp_connection_manager::McpConnectionManager(
                                app_store: Some(store.clone()),
                            ) {
                                MCPStdioServerMenu(
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
    }

    #[test]
    fn stdio_menu_toggle_dispatches_to_mcp_connection_service() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-stdio-menu-toggle-{}",
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
            let mut app = element! { StdioToggleHarness(cancels: cancels_for_app) };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::iter(vec![
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
        assert!(crate::services::mcp::config::is_mcp_server_disabled("docs"));

        std::env::set_current_dir(previous_cwd).unwrap();
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn stdio_menu_renders_official_round_box_body_and_single_custom_footer() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::services::mcp::mcp_connection_manager::McpConnectionManager() {
                    MCPStdioServerMenu(server: Some(server(McpServerConnectionType::Connected)))
                }
            }
        }
        .render(Some(100));
        let text = canvas.to_string();

        assert!(
            text.lines().any(|line| line.starts_with('╭')),
            "canvas=\n{text}"
        );
        assert!(text.contains("Docs MCP Server"), "canvas=\n{text}");
        assert!(
            text.contains(&format!("Status: {} connected", figures::get().tick)),
            "canvas=\n{text}"
        );
        assert!(text.contains("Command: node"), "canvas=\n{text}");
        assert!(text.contains("Reconnect"), "canvas=\n{text}");
        assert!(text.contains("Disable"), "canvas=\n{text}");
        assert_eq!(text.matches("↑↓ to navigate").count(), 1, "canvas=\n{text}");
        assert!(
            text.contains("Enter to select · Esc to back"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn stdio_menu_navigation_wraps_from_first_to_last_like_official_select() {
        let text = futures::executor::block_on(async {
            let mut app = element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    crate::services::mcp::mcp_connection_manager::McpConnectionManager() {
                        MCPStdioServerMenu(server: Some(server(McpServerConnectionType::Connected)))
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::iter(vec![
                        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Up)),
                    ]))
                    .with_size(100, 24),
                ),
            );
            let mut last = String::new();
            for _ in 0..6 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                last = canvas.to_string();
                if last.contains(&format!("{} Disable", figures::get().pointer)) {
                    break;
                }
            }
            last
        });

        assert!(
            text.contains(&format!("{} Disable", figures::get().pointer)),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn stdio_menu_first_ctrl_c_replaces_navigation_footer_with_exit_hint() {
        let text = futures::executor::block_on(async {
            let mut ctrl_c = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('c'));
            ctrl_c.modifiers = KeyModifiers::CONTROL;
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        crate::services::mcp::mcp_connection_manager::McpConnectionManager() {
                            MCPStdioServerMenu(server: Some(server(McpServerConnectionType::Connected)))
                        }
                    }
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::iter(vec![
                        TerminalEvent::Key(ctrl_c),
                    ]))
                    .with_size(100, 24),
                ),
            );
            let mut last = String::new();
            for _ in 0..6 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                last = canvas.to_string();
                if last.contains("Press Ctrl-C again to exit") {
                    break;
                }
            }
            last
        });

        assert!(
            text.contains("Press Ctrl-C again to exit"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("↑↓ to navigate"), "canvas=\n{text}");
    }

    #[test]
    fn stdio_menu_actions_match_official_enabled_disabled_branches() {
        assert_eq!(
            menu_actions_for_stdio_server(&server(McpServerConnectionType::Connected)),
            vec![
                StdioServerMenuAction::Reconnect,
                StdioServerMenuAction::ToggleEnabled
            ]
        );
        assert_eq!(
            menu_actions_for_stdio_server(&server(McpServerConnectionType::Disabled)),
            vec![StdioServerMenuAction::ToggleEnabled]
        );
    }
}
