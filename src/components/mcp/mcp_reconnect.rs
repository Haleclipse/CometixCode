//! Maps to: CC `components/mcp/MCPReconnect.tsx`.
//!
//! Checks the already-known runtime MCP snapshot, invokes the service-owned
//! reconnect callback, updates runtime MCP state, and returns the official
//! reconnect/not-found/failure copy through `on_complete`. Transport lifecycle
//! remains in `services/mcp/*`.

use crate::components::spinner::Spinner;
use crate::constants::figures;
use crate::services::mcp::types::McpServerConnectionType;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct MCPReconnectProps {
    pub server_name: String,
    pub on_complete: Handler<String>,
}

pub fn reconnect_result_from_status(
    server_name: &str,
    status: Option<McpServerConnectionType>,
) -> String {
    match status {
        None => format!("MCP server \"{server_name}\" not found"),
        Some(McpServerConnectionType::Connected) => {
            format!("Successfully reconnected to {server_name}")
        }
        Some(McpServerConnectionType::NeedsAuth) => {
            format!("{server_name} requires authentication. Use /mcp to authenticate.")
        }
        Some(McpServerConnectionType::Pending)
        | Some(McpServerConnectionType::Failed)
        | Some(McpServerConnectionType::Disabled) => {
            format!("Failed to reconnect to {server_name}")
        }
    }
}

pub fn reconnect_error_from_status(
    server_name: &str,
    status: Option<McpServerConnectionType>,
) -> Option<String> {
    match status {
        None => Some(format!("MCP server \"{server_name}\" not found")),
        Some(McpServerConnectionType::Connected) => None,
        Some(McpServerConnectionType::NeedsAuth) => {
            Some(format!("{server_name} requires authentication"))
        }
        Some(McpServerConnectionType::Pending)
        | Some(McpServerConnectionType::Failed)
        | Some(McpServerConnectionType::Disabled) => {
            Some(format!("Failed to reconnect to {server_name}"))
        }
    }
}

#[component]
pub fn MCPReconnect(props: &MCPReconnectProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    // Maps to: CC `MCPReconnect.tsx:23` — `const reconnectMcpServer = useMcpReconnect()`.
    let reconnect_mcp_server =
        crate::services::mcp::mcp_connection_manager::use_mcp_reconnect(&mut hooks);
    let is_reconnecting = hooks.use_state(|| true);
    let error = hooks.use_state(|| Option::<String>::None);
    let mut pending_result = hooks.use_state(|| Option::<String>::None);
    let server_name = props.server_name.clone();

    hooks.use_future({
        let mut is_reconnecting = is_reconnecting;
        let mut error = error;
        let mut pending_result = pending_result;
        async move {
            // CC `:44` — `const result = await reconnectMcpServer(serverName)`.
            let Some(server) = reconnect_mcp_server.call(&server_name).await else {
                let result = reconnect_result_from_status(&server_name, None);
                error.set(reconnect_error_from_status(&server_name, None));
                is_reconnecting.set(false);
                pending_result.set(Some(result));
                return;
            };
            let status = server.client.status;
            let result = reconnect_result_from_status(&server_name, Some(status));
            let error_message = reconnect_error_from_status(&server_name, Some(status));
            error.set(error_message);
            is_reconnecting.set(false);
            pending_result.set(Some(result));
        }
    });

    let pending_result_value = { pending_result.read().clone() };
    if let Some(result) = pending_result_value {
        pending_result.set(None);
        (props.on_complete)(result);
    }

    if is_reconnecting.get() {
        return element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32, row_gap: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Reconnecting to ".to_string(), color: theme.text, wrap: TextWrap::NoWrap)
                    Text(content: props.server_name.clone(), color: theme.text, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Row) {
                    Spinner
                    Text(content: " Establishing connection to MCP server".to_string(), wrap: TextWrap::NoWrap)
                }
            }
        }.into_any();
    }

    if let Some(error_message) = error.read().clone() {
        return element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32, row_gap: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: format!("{} ", figures::get().cross), color: theme.error, wrap: TextWrap::NoWrap)
                    Text(content: format!("Failed to reconnect to {}", props.server_name), color: theme.error, wrap: TextWrap::NoWrap)
                }
                Text(content: format!("Error: {error_message}"), color: theme.inactive, wrap: TextWrap::Wrap)
            }
        }.into_any();
    }

    element! { View {} }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_reconnect_results_match_official_safe_branches() {
        assert_eq!(
            reconnect_result_from_status("docs", None),
            "MCP server \"docs\" not found"
        );
        assert_eq!(
            reconnect_result_from_status("docs", Some(McpServerConnectionType::Connected)),
            "Successfully reconnected to docs"
        );
        assert_eq!(
            reconnect_result_from_status("docs", Some(McpServerConnectionType::NeedsAuth)),
            "docs requires authentication. Use /mcp to authenticate."
        );
        assert_eq!(
            reconnect_result_from_status("docs", Some(McpServerConnectionType::Failed)),
            "Failed to reconnect to docs"
        );
        assert_eq!(
            reconnect_error_from_status("docs", Some(McpServerConnectionType::NeedsAuth)),
            Some("docs requires authentication".to_string())
        );
        assert_eq!(
            reconnect_error_from_status("docs", Some(McpServerConnectionType::Connected)),
            None
        );
    }
}
