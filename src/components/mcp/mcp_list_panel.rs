//! Maps to: CC `components/mcp/MCPListPanel.tsx`.
//!
//! This component preserves the official list grouping, heading labels, status
//! text, parsing diagnostics, empty-null behavior, and cancel output shape.
//! Selection/menu actions remain callback seams; no MCP client operations run here.

use super::mcp_parsing_warnings::McpParsingWarnings;
use super::types::{AgentMcpServerInfo, ServerInfo};
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::design_system::byline::Byline;
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::keyboard_shortcut_hint::{
    KeyboardShortcutHint, KeyboardShortcutHintStyleContext,
};
use crate::constants::figures;
use crate::services::mcp::types::{ConfigScope, McpServerConnectionType};
use crate::services::mcp::utils::describe_mcp_config_file_path;
use iocraft::prelude::*;
use std::collections::BTreeMap;

const SCOPE_ORDER: [ConfigScope; 3] = [ConfigScope::Project, ConfigScope::Local, ConfigScope::User];

#[derive(Default, Props)]
pub struct MCPListPanelProps<'a> {
    pub servers: Vec<ServerInfo>,
    pub agent_servers: Vec<AgentMcpServerInfo>,
    pub on_select_server: HandlerMut<'a, ServerInfo>,
    pub on_select_agent_server: HandlerMut<'a, AgentMcpServerInfo>,
    pub on_complete: Handler<String>,
    /// Maps to official prop; currently retained for the state round-trip when
    /// returning from a server menu. The official component accepts it but does
    /// not use it in the visible list render.
    pub default_tab: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectableMcpItem {
    Server(ServerInfo),
    AgentServer(AgentMcpServerInfo),
}

/// Maps to: CC `components/mcp/MCPListPanel.tsx#getScopeHeading`.
pub fn get_scope_heading(scope: ConfigScope) -> (String, Option<String>) {
    match scope {
        ConfigScope::Project => (
            "Project MCPs".to_string(),
            Some(describe_mcp_config_file_path(scope)),
        ),
        ConfigScope::User => (
            "User MCPs".to_string(),
            Some(describe_mcp_config_file_path(scope)),
        ),
        ConfigScope::Local => (
            "Local MCPs".to_string(),
            Some(describe_mcp_config_file_path(scope)),
        ),
        ConfigScope::Dynamic => (
            "Built-in MCPs".to_string(),
            Some("always available".to_string()),
        ),
    }
}

/// Maps to: CC `groupServersByScope(...)`.
pub fn group_servers_by_scope(servers: &[ServerInfo]) -> BTreeMap<ConfigScope, Vec<ServerInfo>> {
    let mut groups = BTreeMap::<ConfigScope, Vec<ServerInfo>>::new();
    for server in servers {
        groups.entry(server.scope).or_default().push(server.clone());
    }
    for group in groups.values_mut() {
        group.sort_by(|a, b| a.name.cmp(&b.name));
    }
    groups
}

pub fn selectable_servers_in_official_order(
    servers: &[ServerInfo],
    agent_servers: &[AgentMcpServerInfo],
) -> Vec<String> {
    let groups = group_servers_by_scope(servers);
    let mut names = Vec::new();
    for scope in SCOPE_ORDER {
        if let Some(scope_servers) = groups.get(&scope) {
            names.extend(scope_servers.iter().map(|server| server.name.clone()));
        }
    }
    names.extend(agent_servers.iter().map(|server| server.name.clone()));
    if let Some(dynamic_servers) = groups.get(&ConfigScope::Dynamic) {
        names.extend(dynamic_servers.iter().map(|server| server.name.clone()));
    }
    names
}

fn selectable_items_in_official_order(
    servers: &[ServerInfo],
    agent_servers: &[AgentMcpServerInfo],
) -> Vec<SelectableMcpItem> {
    let groups = group_servers_by_scope(servers);
    let mut items = Vec::new();
    for scope in SCOPE_ORDER {
        if let Some(scope_servers) = groups.get(&scope) {
            items.extend(scope_servers.iter().cloned().map(SelectableMcpItem::Server));
        }
    }
    items.extend(
        agent_servers
            .iter()
            .cloned()
            .map(SelectableMcpItem::AgentServer),
    );
    if let Some(dynamic_servers) = groups.get(&ConfigScope::Dynamic) {
        items.extend(
            dynamic_servers
                .iter()
                .cloned()
                .map(SelectableMcpItem::Server),
        );
    }
    items
}

/// Maps to: CC `MCPListPanel.tsx` `[...new Set(agentServers.flatMap(s =>
/// s.sourceAgents))]`: preserve first-seen source-agent order instead of
/// sorting alphabetically.
pub fn agent_source_order(agent_servers: &[AgentMcpServerInfo]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut ordered = Vec::new();
    for server in agent_servers {
        for source in &server.source_agents {
            if seen.insert(source.clone()) {
                ordered.push(source.clone());
            }
        }
    }
    ordered
}

pub fn status_text(status: McpServerConnectionType, reconnect: Option<(u32, u32)>) -> &'static str {
    match status {
        McpServerConnectionType::Disabled => "disabled",
        McpServerConnectionType::Connected => "connected",
        McpServerConnectionType::Pending => {
            if reconnect.is_some() {
                "reconnecting…"
            } else {
                "connecting…"
            }
        }
        McpServerConnectionType::NeedsAuth => "needs authentication",
        McpServerConnectionType::Failed => "failed",
    }
}

fn status_icon(status: McpServerConnectionType) -> &'static str {
    let figures = figures::get();
    match status {
        McpServerConnectionType::Disabled | McpServerConnectionType::Pending => figures.radio_off,
        McpServerConnectionType::Connected => figures.tick,
        McpServerConnectionType::NeedsAuth => figures.triangle_up_outline,
        McpServerConnectionType::Failed => figures.cross,
    }
}

fn server_status_text(server: &ServerInfo) -> String {
    if server.official_client_type() == McpServerConnectionType::Pending
        && let (Some(attempt), Some(max)) =
            (server.reconnect_attempt, server.max_reconnect_attempts)
    {
        return format!("reconnecting ({attempt}/{max})…");
    }
    status_text(server.official_client_type(), None).to_string()
}

fn render_server_item(
    server: ServerInfo,
    index: usize,
    selected_index: usize,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let figures = figures::get();
    let selected = selected_index == index;
    let pointer = if selected {
        format!("{} ", figures.pointer)
    } else {
        "  ".to_string()
    };
    let icon_color = match server.official_client_type() {
        McpServerConnectionType::Connected => theme.success,
        McpServerConnectionType::NeedsAuth => theme.warning,
        McpServerConnectionType::Failed => theme.error,
        McpServerConnectionType::Disabled | McpServerConnectionType::Pending => theme.inactive,
    };
    element! {
        View(flex_direction: FlexDirection::Row) {
            Text(content: pointer, color: if selected { theme.suggestion } else { theme.text }, wrap: TextWrap::NoWrap)
            Text(content: server.name.clone(), color: if selected { theme.suggestion } else { theme.text }, wrap: TextWrap::NoWrap)
            Text(content: " · ".to_string(), dim: !selected, wrap: TextWrap::NoWrap)
            Text(content: status_icon(server.official_client_type()).to_string(), color: icon_color, dim: !selected, wrap: TextWrap::NoWrap)
            Text(content: format!(" {}", server_status_text(&server)), dim: !selected, wrap: TextWrap::NoWrap)
        }
    }.into_any()
}

fn render_agent_server_item(
    agent_server: AgentMcpServerInfo,
    index: usize,
    selected_index: usize,
    theme: crate::utils::theme::Theme,
) -> AnyElement<'static> {
    let figures = figures::get();
    let selected = selected_index == index;
    let pointer = if selected {
        format!("{} ", figures.pointer)
    } else {
        "  ".to_string()
    };
    let status = if agent_server.needs_auth {
        "may need auth"
    } else {
        "agent-only"
    };
    let icon = if agent_server.needs_auth {
        figures.triangle_up_outline
    } else {
        figures.radio_off
    };
    element! {
        View(flex_direction: FlexDirection::Row) {
            Text(content: pointer, color: if selected { theme.suggestion } else { theme.text }, wrap: TextWrap::NoWrap)
            Text(content: agent_server.name.clone(), color: if selected { theme.suggestion } else { theme.text }, wrap: TextWrap::NoWrap)
            Text(content: " · ".to_string(), dim: !selected, wrap: TextWrap::NoWrap)
            Text(content: icon.to_string(), color: if agent_server.needs_auth { theme.warning } else { theme.inactive }, dim: !selected, wrap: TextWrap::NoWrap)
            Text(content: format!(" {status}"), dim: !selected, wrap: TextWrap::NoWrap)
        }
    }.into_any()
}

#[component]
pub fn MCPListPanel<'a>(
    props: &mut MCPListPanelProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<crate::utils::theme::Theme>();
    let mut selected_index = hooks.use_state(|| 0usize);
    let servers = props.servers.clone();
    let agent_servers = props.agent_servers.clone();
    let total_servers = servers.len() + agent_servers.len();

    let selectable_items = selectable_items_in_official_order(&servers, &agent_servers);
    let selectable_count = selectable_items.len().max(1);
    let mut pending_selection = hooks.use_state(|| Option::<SelectableMcpItem>::None);
    let mut pending_complete = hooks.use_state(|| Option::<String>::None);
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let handlers: crate::keybindings::use_keybinding::KeybindingHandlers = vec![
        (
            "confirm:previous".to_string(),
            Box::new(move || {
                let current = selected_index.get();
                selected_index.set(if current == 0 {
                    selectable_count - 1
                } else {
                    current - 1
                });
                true
            }),
        ),
        (
            "confirm:next".to_string(),
            Box::new(move || {
                let current = selected_index.get();
                selected_index.set(if current + 1 >= selectable_count {
                    0
                } else {
                    current + 1
                });
                true
            }),
        ),
        ("confirm:yes".to_string(), {
            let selectable_items = selectable_items.clone();
            Box::new(move || {
                if let Some(item) = selectable_items.get(selected_index.get()) {
                    pending_selection.set(Some(item.clone()));
                }
                true
            })
        }),
        (
            "confirm:no".to_string(),
            Box::new(move || {
                pending_complete.set(Some("MCP dialog dismissed".to_string()));
                true
            }),
        ),
    ];
    crate::keybindings::use_keybinding::use_keybindings(
        &mut hooks,
        runtime,
        handlers,
        crate::keybindings::types::ContextName::Confirmation,
        move || total_servers > 0,
    );

    if total_servers == 0 {
        return element! { View {} };
    }

    let pending_complete_value = { pending_complete.read().clone() };
    if let Some(message) = pending_complete_value {
        pending_complete.set(None);
        (props.on_complete)(message);
    }
    let pending_selection_value = { pending_selection.read().clone() };
    if let Some(selection) = pending_selection_value {
        pending_selection.set(None);
        match selection {
            SelectableMcpItem::Server(server) => (props.on_select_server)(server),
            SelectableMcpItem::AgentServer(agent_server) => {
                (props.on_select_agent_server)(agent_server)
            }
        }
    }

    let groups = group_servers_by_scope(&servers);
    let dynamic_servers = groups
        .get(&ConfigScope::Dynamic)
        .cloned()
        .unwrap_or_default();
    let mut flat_index = 0usize;
    let has_failed_clients = servers
        .iter()
        .any(|server| server.official_client_type() == McpServerConnectionType::Failed);
    let debug_mode =
        std::env::args().any(|arg| arg == "--debug" || arg == "-d" || arg.starts_with("--debug="));

    element! {
        View(flex_direction: FlexDirection::Column) {
            McpParsingWarnings()
            Dialog(
                title: "Manage MCP servers".to_string(),
                subtitle: Some(format!("{total_servers} {}", if total_servers == 1 { "server" } else { "servers" })),
                color: Some(theme.permission),
                hide_input_guide: true,
                on_cancel: move |_| pending_complete.set(Some("MCP dialog dismissed".to_string())),
            ) {
                View(flex_direction: FlexDirection::Column) {
                    #(SCOPE_ORDER.into_iter().filter_map(|scope| {
                        let servers_for_scope = groups.get(&scope)?.clone();
                        let (label, path) = get_scope_heading(scope);
                        let start_index = flat_index;
                        flat_index += servers_for_scope.len();
                        Some(element! {
                            View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                                View(padding_left: 2u32, flex_direction: FlexDirection::Row) {
                                    Text(content: label, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                    #(path.map(|path| element! { Text(content: format!(" ({path})"), dim: true, wrap: TextWrap::NoWrap) }))
                                }
                                #(servers_for_scope.into_iter().enumerate().map(|(offset, server)| {
                                    render_server_item(server, start_index + offset, selected_index.get(), theme)
                                }))
                            }
                        })
                    }))
                    #(if agent_servers.is_empty() { None } else {
                        let start_index = flat_index;
                        flat_index += agent_servers.len();
                        let source_agents = agent_source_order(&agent_servers);
                        Some(element! {
                            View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                                View(padding_left: 2u32) { Text(content: "Agent MCPs".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap) }
                                #(source_agents.into_iter().map(|agent_name| {
                                    let rows = agent_servers.iter().enumerate().filter(|&(_, agent_server)| agent_server.source_agents.contains(&agent_name)).map(|(offset, agent_server)| (offset, agent_server.clone())).collect::<Vec<_>>();
                                    element! {
                                        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                                            View(padding_left: 2u32) { Text(content: format!("@{agent_name}"), dim: true, wrap: TextWrap::NoWrap) }
                                            #(rows.into_iter().map(|(offset, agent_server)| render_agent_server_item(agent_server, start_index + offset, selected_index.get(), theme)))
                                        }
                                    }
                                }))
                            }
                        })
                    })
                    #(if dynamic_servers.is_empty() { None } else {
                        let start_index = flat_index;
                        let heading = get_scope_heading(ConfigScope::Dynamic);
                        Some(element! {
                            View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                                View(padding_left: 2u32, flex_direction: FlexDirection::Row) {
                                    Text(content: heading.0, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                    #(heading.1.map(|path| element! { Text(content: format!(" ({path})"), dim: true, wrap: TextWrap::NoWrap) }))
                                }
                                #(dynamic_servers.into_iter().enumerate().map(|(offset, server)| render_server_item(server, start_index + offset, selected_index.get(), theme)))
                            }
                        })
                    })
                    #(if has_failed_clients {
                        Some(element! { Text(content: if debug_mode { "※ Error logs shown inline with --debug".to_string() } else { "※ Run nyan --debug to see error logs".to_string() }, dim: true, wrap: TextWrap::NoWrap) })
                    } else { None })
                }
            }
            View(padding_left: 1u32, padding_right: 1u32) {
                ContextProvider(value: Context::owned(KeyboardShortcutHintStyleContext { dim: true, italic: true })) {
                    Byline {
                        KeyboardShortcutHint(shortcut: "↑↓".to_string(), action: "navigate".to_string())
                        KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
                        ConfigurableShortcutHint(
                            action: "confirm:no".to_string(),
                            context: "Confirmation".to_string(),
                            fallback: "Esc".to_string(),
                            description: "cancel".to_string(),
                        )
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::mcp::types::mcp_client_state_from_parts;
    use crate::services::mcp::types::{ScopedMcpServerConfig, Transport};
    use crate::utils::theme;

    fn server(name: &str, scope: ConfigScope, status: McpServerConnectionType) -> ServerInfo {
        ServerInfo {
            name: name.to_string(),
            client: mcp_client_state_from_parts(status, Vec::new(), 0, None, None),
            client_type: status,
            scope,
            transport: Transport::Stdio,
            is_authenticated: None,
            config: ScopedMcpServerConfig {
                name: None,
                scope,
                transport: Transport::Stdio,
                command: Some("cmd".to_string()),
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
            },
            reconnect_attempt: None,
            max_reconnect_attempts: None,
            tools: Vec::new(),
            prompts_count: 0,
            resources_count: 0,
        }
    }

    #[test]
    fn mcp_list_panel_helpers_match_official_grouping_and_status_text() {
        let servers = vec![
            server("z", ConfigScope::User, McpServerConnectionType::Connected),
            server("a", ConfigScope::User, McpServerConnectionType::Failed),
            server("p", ConfigScope::Project, McpServerConnectionType::Pending),
        ];
        assert_eq!(
            selectable_servers_in_official_order(&servers, &[]),
            vec!["p", "a", "z"]
        );
        let project_heading = get_scope_heading(ConfigScope::Project);
        assert_eq!(project_heading.0, "Project MCPs");
        assert!(project_heading.1.unwrap().ends_with(".mcp.json"));
        assert_eq!(
            get_scope_heading(ConfigScope::Dynamic),
            (
                "Built-in MCPs".to_string(),
                Some("always available".to_string())
            )
        );
        assert_eq!(
            status_text(McpServerConnectionType::NeedsAuth, None),
            "needs authentication"
        );
    }

    #[test]
    fn mcp_list_panel_status_icons_keep_semantic_colors_and_only_unselected_rows_dim() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                MCPListPanel(
                    servers: vec![
                        server("connected", ConfigScope::Project, McpServerConnectionType::Connected),
                        server("failed", ConfigScope::User, McpServerConnectionType::Failed),
                    ],
                    agent_servers: Vec::new(),
                )
            }
        }
        .render(Some(120));
        let text = canvas.to_string();
        let lines = text.lines().collect::<Vec<_>>();
        let connected_row = lines
            .iter()
            .position(|line| line.contains("connected ·"))
            .unwrap();
        let failed_row = lines
            .iter()
            .position(|line| line.contains("failed ·"))
            .unwrap();
        let connected_icon_col = lines[connected_row]
            .chars()
            .position(|ch| ch.to_string() == figures::get().tick)
            .unwrap();
        let failed_icon_col = lines[failed_row]
            .chars()
            .position(|ch| ch.to_string() == figures::get().cross)
            .unwrap();
        let connected_status_byte = lines[connected_row].rfind("connected").unwrap();
        let connected_status_col = lines[connected_row][..connected_status_byte]
            .chars()
            .count();
        let failed_status_byte = lines[failed_row].rfind("failed").unwrap();
        let failed_status_col = lines[failed_row][..failed_status_byte].chars().count();

        assert_eq!(
            canvas
                .resolved_text_style(connected_icon_col, connected_row)
                .unwrap()
                .color,
            Some(current_theme.success)
        );
        assert_eq!(
            canvas
                .resolved_text_style(failed_icon_col, failed_row)
                .unwrap()
                .color,
            Some(current_theme.error)
        );
        assert_ne!(
            canvas
                .resolved_text_style(connected_status_col, connected_row)
                .unwrap()
                .weight,
            Weight::Light,
            "selected status must not be dim: canvas=\n{text}"
        );
        assert_eq!(
            canvas
                .resolved_text_style(failed_status_col, failed_row)
                .unwrap()
                .weight,
            Weight::Light,
            "unselected status must be dim: canvas=\n{text}"
        );
    }

    #[test]
    fn mcp_list_panel_agent_source_order_preserves_official_first_seen_order() {
        let first = AgentMcpServerInfo {
            name: "shared".to_string(),
            transport: Transport::Http,
            url: Some("https://example.com".to_string()),
            command: None,
            source_agents: vec!["zeta".to_string(), "alpha".to_string()],
            needs_auth: true,
            is_authenticated: false,
        };
        let second = AgentMcpServerInfo {
            name: "other".to_string(),
            transport: Transport::Stdio,
            url: None,
            command: Some("server".to_string()),
            source_agents: vec!["alpha".to_string(), "beta".to_string()],
            needs_auth: false,
            is_authenticated: false,
        };
        assert_eq!(
            agent_source_order(&[first, second]),
            vec!["zeta".to_string(), "alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn mcp_list_panel_renders_official_title_headings_and_statuses() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                MCPListPanel(
                    servers: vec![
                        server("filesystem", ConfigScope::Project, McpServerConnectionType::Pending),
                        server("github", ConfigScope::User, McpServerConnectionType::Disabled),
                    ],
                    agent_servers: Vec::new(),
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains("Manage MCP servers"), "canvas=\n{text}");
        assert!(text.contains("2 servers"), "canvas=\n{text}");
        assert!(text.contains("Project MCPs"), "canvas=\n{text}");
        assert!(text.contains("filesystem"), "canvas=\n{text}");
        assert!(text.contains("connecting"), "canvas=\n{text}");
        assert!(text.contains("User MCPs"), "canvas=\n{text}");
        assert!(text.contains("github"), "canvas=\n{text}");
        assert!(text.contains("disabled"), "canvas=\n{text}");
    }
}
