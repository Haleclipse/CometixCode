//! Maps to: CC `hooks/notifs/useIDEStatusIndicator.tsx`.
//!
//! The official hook can detect IDEs and increment a persisted hint counter.
//! Nyan keeps this as a pure producer over an already-known IDE snapshot: no
//! IDE probing, no MCP client work, no config writes, and no terminal overlay.

use crate::context::notifications::{
    Notification, NotificationColor, NotificationPriority, NotificationSegment,
};

pub const IDE_STATUS_HINT_KEY: &str = "ide-status-hint";
pub const IDE_STATUS_DISCONNECTED_KEY: &str = "ide-status-disconnected";
pub const IDE_STATUS_JETBRAINS_DISCONNECTED_KEY: &str = "ide-status-jetbrains-disconnected";
pub const IDE_STATUS_INSTALL_ERROR_KEY: &str = "ide-status-install-error";
pub const MAX_IDE_HINT_SHOW_COUNT: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdeConnectionStatus {
    Connected,
    Disconnected,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdeStatusSnapshot {
    pub is_remote_mode: bool,
    pub is_supported_terminal: bool,
    pub ide_status: Option<IdeConnectionStatus>,
    pub ide_name: Option<String>,
    pub ide_install_error: bool,
    pub is_jetbrains_ide: bool,
    pub ide_selection_active: bool,
    pub ide_hint_shown_count: u32,
    pub has_shown_hint_this_session: bool,
    pub detected_ide_name: Option<String>,
}

pub fn ide_status_notifications(snapshot: &IdeStatusSnapshot) -> Vec<Notification> {
    if snapshot.is_remote_mode {
        return Vec::new();
    }

    let should_show_ide_selection = snapshot.ide_status == Some(IdeConnectionStatus::Connected)
        && snapshot.ide_selection_active;
    let should_show_connected =
        snapshot.ide_status == Some(IdeConnectionStatus::Connected) && !should_show_ide_selection;
    let show_install_error_or_jetbrains_info =
        snapshot.ide_install_error || snapshot.is_jetbrains_ide;
    let show_ide_install_error = show_install_error_or_jetbrains_info
        && !snapshot.is_jetbrains_ide
        && !should_show_connected
        && !should_show_ide_selection;
    let show_jetbrains_info = show_install_error_or_jetbrains_info
        && snapshot.is_jetbrains_ide
        && !should_show_connected
        && !should_show_ide_selection;

    let mut notifications = Vec::new();

    if show_ide_install_error {
        notifications.push(ide_status_install_error_notification());
    }

    if show_jetbrains_info {
        notifications.push(ide_status_jetbrains_disconnected_notification());
    }

    if !show_ide_install_error
        && !show_jetbrains_info
        && snapshot.ide_status == Some(IdeConnectionStatus::Disconnected)
        && snapshot.ide_name.is_some()
    {
        notifications.push(ide_status_disconnected_notification(
            snapshot.ide_name.as_deref().expect("checked above"),
        ));
    }

    if let Some(ide_name) = ide_status_hint_candidate(snapshot, show_jetbrains_info) {
        notifications.push(ide_status_hint_notification(ide_name));
    }

    notifications
}

pub fn ide_status_hint_candidate(
    snapshot: &IdeStatusSnapshot,
    show_jetbrains_info: bool,
) -> Option<&str> {
    if snapshot.is_remote_mode
        || snapshot.is_supported_terminal
        || snapshot.ide_status.is_some()
        || show_jetbrains_info
        || snapshot.has_shown_hint_this_session
        || snapshot.ide_hint_shown_count >= MAX_IDE_HINT_SHOW_COUNT
    {
        return None;
    }

    snapshot.detected_ide_name.as_deref()
}

pub fn ide_status_hint_notification(ide_name: &str) -> Notification {
    Notification::text(
        IDE_STATUS_HINT_KEY,
        format!("/ide for {ide_name}"),
        NotificationPriority::Low,
    )
    .with_segments(vec![
        NotificationSegment::text("/ide for ").with_dim(true),
        NotificationSegment::text(ide_name).with_color(NotificationColor::Ide),
    ])
}

pub fn ide_status_disconnected_notification(ide_name: &str) -> Notification {
    Notification::text(
        IDE_STATUS_DISCONNECTED_KEY,
        format!("{ide_name} disconnected"),
        NotificationPriority::Medium,
    )
    .with_color(NotificationColor::Error)
    .with_invalidates(vec![
        IDE_STATUS_HINT_KEY.to_string(),
        IDE_STATUS_INSTALL_ERROR_KEY.to_string(),
        IDE_STATUS_JETBRAINS_DISCONNECTED_KEY.to_string(),
    ])
}

pub fn ide_status_jetbrains_disconnected_notification() -> Notification {
    Notification::text(
        IDE_STATUS_JETBRAINS_DISCONNECTED_KEY,
        "IDE plugin not connected · /status for info",
        NotificationPriority::Medium,
    )
    .with_invalidates(vec![
        IDE_STATUS_HINT_KEY.to_string(),
        IDE_STATUS_DISCONNECTED_KEY.to_string(),
        IDE_STATUS_INSTALL_ERROR_KEY.to_string(),
    ])
}

pub fn ide_status_install_error_notification() -> Notification {
    Notification::text(
        IDE_STATUS_INSTALL_ERROR_KEY,
        "IDE extension install failed (see /status for info)",
        NotificationPriority::Medium,
    )
    .with_color(NotificationColor::Error)
    .with_invalidates(vec![
        IDE_STATUS_HINT_KEY.to_string(),
        IDE_STATUS_DISCONNECTED_KEY.to_string(),
        IDE_STATUS_JETBRAINS_DISCONNECTED_KEY.to_string(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> IdeStatusSnapshot {
        IdeStatusSnapshot::default()
    }

    #[test]
    fn ide_status_hint_matches_official_external_terminal_copy() {
        let notification = ide_status_hint_notification("VS Code");

        assert_eq!(notification.key, IDE_STATUS_HINT_KEY);
        assert_eq!(notification.text, "/ide for VS Code");
        assert_eq!(notification.priority, NotificationPriority::Low);
        assert_eq!(notification.segments.len(), 2);
        assert_eq!(notification.segments[0].text, "/ide for ");
        assert!(notification.segments[0].dim);
        assert_eq!(notification.segments[1].text, "VS Code");
        assert_eq!(notification.segments[1].color, Some(NotificationColor::Ide));
    }

    #[test]
    fn ide_status_hint_is_gated_without_probe_or_config_write() {
        let mut snap = snapshot();
        snap.detected_ide_name = Some("VS Code".to_string());
        assert_eq!(ide_status_notifications(&snap).len(), 1);

        snap.is_supported_terminal = true;
        assert!(ide_status_notifications(&snap).is_empty());

        snap.is_supported_terminal = false;
        snap.ide_hint_shown_count = MAX_IDE_HINT_SHOW_COUNT;
        assert!(ide_status_notifications(&snap).is_empty());

        snap.ide_hint_shown_count = 0;
        snap.has_shown_hint_this_session = true;
        assert!(ide_status_notifications(&snap).is_empty());
    }

    #[test]
    fn ide_status_disconnected_matches_official_copy_and_color() {
        let mut snap = snapshot();
        snap.ide_status = Some(IdeConnectionStatus::Disconnected);
        snap.ide_name = Some("Cursor".to_string());

        let notifications = ide_status_notifications(&snap);

        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].key, IDE_STATUS_DISCONNECTED_KEY);
        assert_eq!(notifications[0].text, "Cursor disconnected");
        assert_eq!(notifications[0].priority, NotificationPriority::Medium);
        assert_eq!(notifications[0].color, Some(NotificationColor::Error));
    }

    #[test]
    fn ide_status_install_error_suppresses_disconnected_notification() {
        let mut snap = snapshot();
        snap.ide_status = Some(IdeConnectionStatus::Disconnected);
        snap.ide_name = Some("Cursor".to_string());
        snap.ide_install_error = true;

        let notifications = ide_status_notifications(&snap);

        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].key, IDE_STATUS_INSTALL_ERROR_KEY);
        assert_eq!(
            notifications[0].text,
            "IDE extension install failed (see /status for info)"
        );
        assert_eq!(notifications[0].color, Some(NotificationColor::Error));
    }

    #[test]
    fn ide_status_jetbrains_info_matches_official_copy() {
        let mut snap = snapshot();
        snap.is_jetbrains_ide = true;

        let notifications = ide_status_notifications(&snap);

        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].key, IDE_STATUS_JETBRAINS_DISCONNECTED_KEY);
        assert_eq!(
            notifications[0].text,
            "IDE plugin not connected · /status for info"
        );
        assert_eq!(notifications[0].priority, NotificationPriority::Medium);
        assert_eq!(notifications[0].color, None);
    }

    #[test]
    fn ide_status_connected_or_selection_states_emit_no_status_warning() {
        let mut snap = snapshot();
        snap.ide_status = Some(IdeConnectionStatus::Connected);
        snap.ide_name = Some("VS Code".to_string());
        snap.ide_install_error = true;
        assert!(ide_status_notifications(&snap).is_empty());

        snap.ide_selection_active = true;
        assert!(ide_status_notifications(&snap).is_empty());
    }

    #[test]
    fn ide_status_remote_mode_suppresses_all_notifications() {
        let mut snap = snapshot();
        snap.is_remote_mode = true;
        snap.ide_install_error = true;
        snap.detected_ide_name = Some("VS Code".to_string());

        assert!(ide_status_notifications(&snap).is_empty());
    }
}
