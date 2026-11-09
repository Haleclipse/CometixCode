//! Maps to: CC `hooks/notifs/useMcpConnectivityStatus.tsx`.
//!
//! Nyan keeps this as a pure producer seam. It consumes an already-known MCP
//! connection snapshot and emits main-screen runtime notifications; it never
//! starts MCP clients, performs auth, or writes settings.

use crate::context::notifications::{
    Notification, NotificationColor, NotificationPriority, NotificationSegment,
};

pub const MCP_FAILED_KEY: &str = "mcp-failed";
pub const MCP_NEEDS_AUTH_KEY: &str = "mcp-needs-auth";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpConnectionStatus {
    Connected,
    Failed,
    NeedsAuth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpConfigType {
    Local,
    SseIde,
    WsIde,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConnectivityClient {
    pub name: String,
    pub status: McpConnectionStatus,
    pub config_type: McpConfigType,
}

impl McpConnectivityClient {
    pub fn new(
        name: impl Into<String>,
        status: McpConnectionStatus,
        config_type: McpConfigType,
    ) -> Self {
        Self {
            name: name.into(),
            status,
            config_type,
        }
    }
}

pub fn mcp_connectivity_notifications(
    clients: &[McpConnectivityClient],
    is_remote_mode: bool,
) -> Vec<Notification> {
    if is_remote_mode {
        return Vec::new();
    }

    let failed_local = clients
        .iter()
        .filter(|client| {
            client.status == McpConnectionStatus::Failed
                && !matches!(
                    client.config_type,
                    McpConfigType::SseIde | McpConfigType::WsIde
                )
        })
        .count();
    let needs_auth_local = clients
        .iter()
        .filter(|client| client.status == McpConnectionStatus::NeedsAuth)
        .count();

    let mut notifications = Vec::new();
    if failed_local > 0 {
        let noun = if failed_local == 1 {
            "server"
        } else {
            "servers"
        };
        notifications.push(mcp_segment_notification(
            MCP_FAILED_KEY,
            format!("{failed_local} MCP {noun} failed"),
            NotificationColor::Error,
        ));
    }
    if needs_auth_local > 0 {
        let auth_copy = if needs_auth_local == 1 {
            "server needs auth"
        } else {
            "servers need auth"
        };
        notifications.push(mcp_segment_notification(
            MCP_NEEDS_AUTH_KEY,
            format!("{needs_auth_local} MCP {auth_copy}"),
            NotificationColor::Warning,
        ));
    }

    notifications
}

fn mcp_segment_notification(
    key: &'static str,
    heading: String,
    color: NotificationColor,
) -> Notification {
    let text = format!("{heading} · /mcp");
    Notification::text(key, text, NotificationPriority::Medium).with_segments(vec![
        NotificationSegment::text(heading).with_color(color),
        NotificationSegment::text(" · /mcp").with_dim(true),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_connectivity_notifications_match_official_failed_local_payload() {
        let notifications = mcp_connectivity_notifications(
            &[
                McpConnectivityClient::new(
                    "local-a",
                    McpConnectionStatus::Failed,
                    McpConfigType::Local,
                ),
                McpConnectivityClient::new(
                    "local-b",
                    McpConnectionStatus::Failed,
                    McpConfigType::Local,
                ),
                McpConnectivityClient::new(
                    "ide",
                    McpConnectionStatus::Failed,
                    McpConfigType::SseIde,
                ),
            ],
            false,
        );

        assert_eq!(notifications.len(), 1);
        let notification = &notifications[0];
        assert_eq!(notification.key, MCP_FAILED_KEY);
        assert_eq!(notification.text, "2 MCP servers failed · /mcp");
        assert_eq!(notification.priority, NotificationPriority::Medium);
        assert_eq!(notification.color, None);
        assert_eq!(notification.segments.len(), 2);
        assert_eq!(notification.segments[0].text, "2 MCP servers failed");
        assert_eq!(
            notification.segments[0].color,
            Some(NotificationColor::Error)
        );
        assert_eq!(notification.segments[1].text, " · /mcp");
        assert!(notification.segments[1].dim);
    }

    #[test]
    fn mcp_connectivity_notifications_match_official_auth_copy() {
        let notifications = mcp_connectivity_notifications(
            &[
                McpConnectivityClient::new(
                    "needs-a",
                    McpConnectionStatus::NeedsAuth,
                    McpConfigType::Local,
                ),
                McpConnectivityClient::new(
                    "needs-b",
                    McpConnectionStatus::NeedsAuth,
                    McpConfigType::WsIde,
                ),
            ],
            false,
        );

        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].key, MCP_NEEDS_AUTH_KEY);
        assert_eq!(notifications[0].text, "2 MCP servers need auth · /mcp");
        assert_eq!(
            notifications[0].segments[0].color,
            Some(NotificationColor::Warning)
        );
    }

    #[test]
    fn mcp_connectivity_notifications_suppress_remote_and_clean_states() {
        let clients = [McpConnectivityClient::new(
            "ok",
            McpConnectionStatus::Connected,
            McpConfigType::Local,
        )];
        assert!(mcp_connectivity_notifications(&clients, false).is_empty());
        assert!(
            mcp_connectivity_notifications(
                &[McpConnectivityClient::new(
                    "bad",
                    McpConnectionStatus::Failed,
                    McpConfigType::Local,
                )],
                true,
            )
            .is_empty()
        );
    }
}
