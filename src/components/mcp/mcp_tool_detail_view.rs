//! Maps to: CC `components/mcp/MCPToolDetailView.tsx`.
//!
//! The description/schema are rendered from already-known metadata; no tool
//! description async call or MCP request is executed here.

use super::types::{McpToolInfo, ServerInfo};
use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::design_system::dialog::Dialog;
use crate::services::mcp::mcp_string_utils::{extract_mcp_tool_display_name, get_mcp_display_name};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct MCPToolDetailViewProps<'a> {
    pub tool: Option<McpToolInfo>,
    pub server: Option<ServerInfo>,
    pub on_back: HandlerMut<'a, ()>,
}

pub fn tool_title_parts(tool: &McpToolInfo, server_name: &str) -> (String, Vec<&'static str>) {
    let tool_name = get_mcp_display_name(&tool.name, server_name);
    let full_display_name = tool.user_facing_name.as_deref().unwrap_or(&tool_name);
    let display_name = extract_mcp_tool_display_name(full_display_name);
    let mut annotations = Vec::new();
    if tool.is_read_only {
        annotations.push("[read-only]");
    }
    if tool.is_destructive {
        annotations.push("[destructive]");
    }
    if tool.is_open_world {
        annotations.push("[open-world]");
    }
    (display_name, annotations)
}

#[component]
pub fn MCPToolDetailView<'a>(
    props: &mut MCPToolDetailViewProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let Some(tool) = props.tool.clone() else {
        return element! { View {} }.into_any();
    };
    let Some(server) = props.server.clone() else {
        return element! { View {} }.into_any();
    };
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let (display_name, _) = tool_title_parts(&tool, &server.name);
    let tool_name = get_mcp_display_name(&tool.name, &server.name);
    let mut title_children = vec![element! {
        Text(content: display_name, color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap)
    }
    .into_any()];
    if tool.is_read_only {
        title_children.push(element! {
            Text(content: " [read-only]".to_string(), color: theme.success, weight: Weight::Bold, wrap: TextWrap::NoWrap)
        }.into_any());
    }
    if tool.is_destructive {
        title_children.push(element! {
            Text(content: " [destructive]".to_string(), color: theme.error, weight: Weight::Bold, wrap: TextWrap::NoWrap)
        }.into_any());
    }
    if tool.is_open_world {
        title_children.push(element! {
            Text(content: " [open-world]".to_string(), color: theme.permission, weight: Weight::Bold, dim: true, wrap: TextWrap::NoWrap)
        }.into_any());
    }
    let mut pending_back = hooks.use_state(|| false);
    if pending_back.get() {
        pending_back.set(false);
        (props.on_back)(());
    }

    element! {
        Dialog(
            title: String::new(),
            title_children: title_children,
            subtitle: Some(server.name.clone()),
            input_guide_children: vec![element! {
                ConfigurableShortcutHint(
                    action: "confirm:no".to_string(),
                    context: "Confirmation".to_string(),
                    fallback: "Esc".to_string(),
                    description: "go back".to_string(),
                )
            }.into_any()],
            on_cancel: move |_| pending_back.set(true),
        ) {
            View(flex_direction: FlexDirection::Column) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Tool name: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: tool_name, dim: true, wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Full name: ".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: tool.name.clone(), dim: true, wrap: TextWrap::NoWrap)
                }
                #(tool.description.as_ref().filter(|description| !description.is_empty()).map(|description| element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "Description:".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: description.clone(), wrap: TextWrap::Wrap)
                    }
                }))
                #(if tool.parameters.is_empty() {
                    None
                } else {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                            Text(content: "Parameters:".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                            View(margin_left: 2u32, flex_direction: FlexDirection::Column) {
                                #(tool.parameters.iter().map(|param| element! {
                                    View(flex_direction: FlexDirection::Row) {
                                        Text(content: format!("• {}", param.name), wrap: TextWrap::NoWrap)
                                        #(if param.required {
                                            Some(element! { Text(content: " (required)".to_string(), dim: true, wrap: TextWrap::NoWrap) })
                                        } else { None })
                                        Text(content: ": ".to_string(), wrap: TextWrap::NoWrap)
                                        Text(content: param.type_name.clone(), dim: true, wrap: TextWrap::NoWrap)
                                        #(param.description.as_ref().map(|description| element! {
                                            Text(content: format!(" - {description}"), dim: true, wrap: TextWrap::Wrap)
                                        }))
                                    }
                                }))
                            }
                        }
                    })
                })
            }
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail_server() -> ServerInfo {
        ServerInfo {
            name: "docs".to_string(),
            client: super::super::types::mcp_client_state_from_parts(
                crate::services::mcp::types::McpServerConnectionType::Connected,
                Vec::new(),
                0,
                None,
                None,
            ),
            client_type: crate::services::mcp::types::McpServerConnectionType::Connected,
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
                url: Some("https://example.com".to_string()),
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
    fn tool_title_parts_match_official_annotations_order() {
        let (name, annotations) = tool_title_parts(
            &McpToolInfo {
                name: "mcp__docs__search".to_string(),
                user_facing_name: Some("docs - Search (MCP)".to_string()),
                description: None,
                is_read_only: true,
                is_destructive: true,
                is_open_world: true,
                parameters: Vec::new(),
            },
            "docs",
        );
        assert_eq!(name, "Search");
        assert_eq!(
            annotations,
            vec!["[read-only]", "[destructive]", "[open-world]"]
        );
    }

    #[test]
    fn tool_detail_renders_semantic_annotation_colors_and_back_only_footer() {
        let current_theme = *crate::utils::theme::current();
        let tool = McpToolInfo {
            name: "mcp__docs__search".to_string(),
            user_facing_name: Some("docs - Search (MCP)".to_string()),
            description: Some("Search docs".to_string()),
            is_read_only: true,
            is_destructive: true,
            is_open_world: true,
            parameters: Vec::new(),
        };
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                MCPToolDetailView(tool: Some(tool), server: Some(detail_server()))
            }
        }
        .render(Some(100));
        let text = canvas.to_string();
        let lines = text.lines().collect::<Vec<_>>();
        let title_row = lines
            .iter()
            .position(|line| line.contains("[read-only]"))
            .unwrap();
        let read_only_byte = lines[title_row].find("[read-only]").unwrap();
        let destructive_byte = lines[title_row].find("[destructive]").unwrap();
        let open_world_byte = lines[title_row].find("[open-world]").unwrap();
        let column = |byte| lines[title_row][..byte].chars().count();

        assert_eq!(
            canvas
                .resolved_text_style(column(read_only_byte), title_row)
                .unwrap()
                .color,
            Some(current_theme.success)
        );
        assert_eq!(
            canvas
                .resolved_text_style(column(destructive_byte), title_row)
                .unwrap()
                .color,
            Some(current_theme.error)
        );
        assert_eq!(
            canvas
                .resolved_text_style(column(open_world_byte), title_row)
                .unwrap()
                .weight,
            Weight::Light
        );
        assert!(text.contains("Esc to go back"), "canvas=\n{text}");
        assert!(!text.contains("Enter to confirm"), "canvas=\n{text}");
    }
}
