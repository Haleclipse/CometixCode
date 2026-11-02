//! Maps to: CC `components/mcp/MCPToolListView.tsx`.
//!
//! This is a read-only view over already-known MCP tool metadata. It does not
//! discover tools or call MCP clients.

use super::types::{McpToolInfo, ServerInfo, mcp_client_state_from_parts};
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::dialog::Dialog;
use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use crate::services::mcp::mcp_string_utils::{extract_mcp_tool_display_name, get_mcp_display_name};
use crate::services::mcp::types::McpServerConnectionType;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct MCPToolListViewProps<'a> {
    pub server: Option<ServerInfo>,
    pub on_select_tool: HandlerMut<'a, usize>,
    pub on_back: HandlerMut<'a, ()>,
}

/// Maps to: CC `MCPToolListView.tsx` `serverTools` memo: disconnected
/// servers show no tools even if a stale menu snapshot still carries them.
pub fn tools_for_tool_list_server(server: &ServerInfo) -> Vec<McpToolInfo> {
    if server.official_client_type() != McpServerConnectionType::Connected {
        Vec::new()
    } else {
        server.tools.clone()
    }
}

pub fn tool_option_for_server(
    tool: &McpToolInfo,
    server_name: &str,
    index: usize,
) -> SelectOptionData {
    let tool_name = get_mcp_display_name(&tool.name, server_name);
    let full_display_name = tool.user_facing_name.as_deref().unwrap_or(&tool_name);
    let display_name = extract_mcp_tool_display_name(full_display_name);

    let mut annotations = Vec::new();
    if tool.is_read_only {
        annotations.push("read-only");
    }
    if tool.is_destructive {
        annotations.push("destructive");
    }
    if tool.is_open_world {
        annotations.push("open-world");
    }

    SelectOptionData {
        label: display_name,
        value: index.to_string(),
        description: (!annotations.is_empty()).then(|| annotations.join(", ")),
        dim_description: false,
        disabled: false,
        input: None,
    }
}

#[component]
pub fn MCPToolListView<'a>(
    props: &mut MCPToolListViewProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let server = props.server.clone().unwrap_or_else(|| ServerInfo {
        name: String::new(),
        client: mcp_client_state_from_parts(
            crate::services::mcp::types::McpServerConnectionType::Pending,
            Vec::new(),
            0,
            None,
            None,
        ),
        client_type: crate::services::mcp::types::McpServerConnectionType::Pending,
        scope: crate::services::mcp::types::ConfigScope::Project,
        transport: crate::services::mcp::types::Transport::Stdio,
        is_authenticated: None,
        config: crate::services::mcp::types::ScopedMcpServerConfig {
            name: None,
            scope: crate::services::mcp::types::ConfigScope::Project,
            transport: crate::services::mcp::types::Transport::Stdio,
            command: None,
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
        },
        reconnect_attempt: None,
        max_reconnect_attempts: None,
        tools: Vec::new(),
        prompts_count: 0,
        resources_count: 0,
    });
    let tools = tools_for_tool_list_server(&server);
    let tool_count = tools.len();
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_select = hooks.use_state(|| Option::<usize>::None);
    let mut pending_back = hooks.use_state(|| false);

    hooks.use_propagated_terminal_events({
        let tools = tools.clone();
        move |event| match event.event() {
            TerminalEvent::Key(KeyEvent { code, kind, .. }) if *kind != KeyEventKind::Release => {
                match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        let count = tools.len().max(1);
                        let current = focused_index.get();
                        focused_index.set(if current == 0 { count - 1 } else { current - 1 });
                        event.stop_propagation();
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        focused_index.set((focused_index.get() + 1) % tools.len().max(1));
                        event.stop_propagation();
                    }
                    KeyCode::Enter => {
                        if focused_index.get() < tools.len() {
                            pending_select.set(Some(focused_index.get()));
                        }
                        event.stop_propagation();
                    }
                    KeyCode::Esc => {
                        pending_back.set(true);
                        event.stop_propagation();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    });

    if pending_back.get() {
        pending_back.set(false);
        (props.on_back)(());
    }
    let pending_select_value = { *pending_select.read() };
    if let Some(index) = pending_select_value {
        pending_select.set(None);
        (props.on_select_tool)(index);
    }

    let mut pending_dialog_back = hooks.use_state(|| false);
    if pending_dialog_back.get() {
        pending_dialog_back.set(false);
        (props.on_back)(());
    }

    let options = tools
        .iter()
        .enumerate()
        .map(|(index, tool)| tool_option_for_server(tool, &server.name, index))
        .collect::<Vec<_>>();

    element! {
        Dialog(
            title: format!("Tools for {}", server.name),
            subtitle: Some(format!("{tool_count} {}", if tool_count == 1 { "tool" } else { "tools" })),
            hide_input_guide: false,
            input_guide_children: vec![element! {
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
            }.into_any()],
            on_cancel: move |_| pending_dialog_back.set(true),
        ) {
            #(if options.is_empty() {
                Some(element! { Text(content: "No tools available".to_string(), dim: true, wrap: TextWrap::NoWrap) }.into_any())
            } else {
                Some(element! {
                    Select(
                        options: options,
                        focused_index: focused_index.get().min(tool_count.saturating_sub(1)),
                        visible_option_count: 5usize,
                        layout: SelectLayout::Compact,
                        hide_indexes: true,
                    )
                }.into_any())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> McpToolInfo {
        McpToolInfo {
            name: "mcp__github__add_comment".to_string(),
            user_facing_name: Some("github - Add comment (MCP)".to_string()),
            description: None,
            is_read_only: true,
            is_destructive: false,
            is_open_world: true,
            parameters: Vec::new(),
        }
    }

    fn server_with_status(status: McpServerConnectionType) -> ServerInfo {
        let tools = vec![tool()];
        ServerInfo {
            name: "github".to_string(),
            client: mcp_client_state_from_parts(status, tools.clone(), 0, None, None),
            client_type: status,
            scope: crate::services::mcp::types::ConfigScope::Project,
            transport: crate::services::mcp::types::Transport::Http,
            is_authenticated: Some(true),
            config: crate::services::mcp::types::ScopedMcpServerConfig {
                name: None,
                scope: crate::services::mcp::types::ConfigScope::Project,
                transport: crate::services::mcp::types::Transport::Http,
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
            tools,
            prompts_count: 0,
            resources_count: 0,
        }
    }

    #[test]
    fn tool_options_extract_display_name_and_annotations_like_official() {
        let option = tool_option_for_server(&tool(), "github", 0);
        assert_eq!(option.label, "Add comment");
        assert_eq!(option.description.as_deref(), Some("read-only, open-world"));
    }

    #[test]
    fn tool_list_uses_official_five_row_viewport_and_navigation_footer() {
        let mut server = server_with_status(McpServerConnectionType::Connected);
        server.tools = (0..7)
            .map(|index| McpToolInfo {
                name: format!("mcp__github__tool_{index}"),
                user_facing_name: None,
                description: None,
                is_read_only: false,
                is_destructive: false,
                is_open_world: false,
                parameters: Vec::new(),
            })
            .collect();
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                MCPToolListView(server: Some(server))
            }
        }
        .render(Some(100));
        let text = canvas.to_string();

        assert!(text.contains("Tools for github"), "canvas=\n{text}");
        assert!(text.contains("7 tools"), "canvas=\n{text}");
        assert!(text.contains("tool_0"), "canvas=\n{text}");
        assert!(text.contains("tool_4"), "canvas=\n{text}");
        assert!(!text.contains("tool_5"), "canvas=\n{text}");
        assert!(
            text.contains("↑↓ to navigate · Enter to select · Esc to back"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn tool_list_hides_stale_tools_when_server_is_not_connected_like_official() {
        assert_eq!(
            tools_for_tool_list_server(&server_with_status(McpServerConnectionType::Connected))
                .len(),
            1
        );
        assert!(
            tools_for_tool_list_server(&server_with_status(McpServerConnectionType::Failed))
                .is_empty()
        );
        assert!(
            tools_for_tool_list_server(&server_with_status(McpServerConnectionType::Disabled))
                .is_empty()
        );
    }
}
