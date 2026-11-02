//! Maps to official startup notification assembly in `main.tsx` plus the
//! startup-oriented notification hooks. This keeps Nyan notification
//! producers read-only and main-screen-safe.

use super::settings_errors::settings_errors_notification_from_errors;
use crate::context::notifications::{
    Notification, NotificationColor, NotificationPriority, NotificationsState,
};
use crate::utils::auto_updater::auto_update_message_for_platform;
use crate::utils::config::is_auto_updater_disabled;
use crate::utils::sandbox::sandbox_adapter::sandbox_unavailable_message_for_platform;
use crate::utils::settings::{SettingsJson, ValidationError};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartupNotificationState {
    pub has_run: bool,
}

pub fn apply_startup_notifications_once(
    is_remote_mode: bool,
    state: &mut StartupNotificationState,
    notifications: impl IntoIterator<Item = Notification>,
) -> NotificationsState {
    if is_remote_mode || state.has_run {
        return NotificationsState::default();
    }

    state.has_run = true;
    // Maps to: CC main.tsx initialState { current: null, queue: initialNotifications }.
    // Seeding does not execute addNotification policy or start a timer before mount.
    NotificationsState {
        current: None,
        queue: notifications.into_iter().collect(),
    }
}

pub fn startup_notifications(
    settings: &SettingsJson,
    errors: &[ValidationError],
) -> NotificationsState {
    let mut state = StartupNotificationState::default();
    startup_notifications_with_gate(settings, errors, false, &mut state)
}

fn auto_update_startup_notification_for_platform(
    platform: &str,
    disabled: bool,
) -> Option<Notification> {
    if disabled {
        return None;
    }
    auto_update_message_for_platform(platform).map(|message| {
        Notification::text(
            "windows-auto-updater-unavailable",
            message,
            NotificationPriority::Low,
        )
    })
}

pub fn startup_notifications_with_gate(
    settings: &SettingsJson,
    errors: &[ValidationError],
    is_remote_mode: bool,
    state: &mut StartupNotificationState,
) -> NotificationsState {
    let mut notifications = settings_errors_notification_from_errors(errors)
        .into_iter()
        .collect::<Vec<_>>();
    let platform = std::env::consts::OS;
    if platform == "windows"
        && let Some(notification) =
            auto_update_startup_notification_for_platform(platform, is_auto_updater_disabled())
    {
        notifications.push(notification);
    }
    if let Some(message) = sandbox_unavailable_message_for_platform(
        crate::utils::platform::get_platform().as_str(),
        crate::utils::sandbox::sandbox_adapter::get_sandbox_enabled_setting(settings),
    ) {
        notifications.push(
            Notification::text(
                "windows-sandbox-unavailable",
                message,
                NotificationPriority::High,
            )
            .with_color(NotificationColor::Warning),
        );
    }
    apply_startup_notifications_once(is_remote_mode, state, notifications)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::notifications::{Notification, NotificationPriority};
    use crate::hooks::notifs::settings_errors::SETTINGS_ERRORS_NOTIFICATION_KEY;

    fn validation_error(path: &str) -> ValidationError {
        ValidationError {
            file: Some("settings.json".to_string()),
            path: path.to_string(),
            message: "invalid setting".to_string(),
            expected: None,
            invalid_value: None,
            doc_link: None,
            suggestion: None,
        }
    }

    #[test]
    fn windows_auto_update_startup_notification_is_low_priority_and_disabled_aware() {
        let notification = auto_update_startup_notification_for_platform("windows", false)
            .expect("enabled Windows updater should produce a startup notification");
        assert_eq!(notification.key, "windows-auto-updater-unavailable");
        assert_eq!(
            notification.text,
            "Windows 暂不支持自动更新，请从 GitHub Releases 手动下载"
        );
        assert_eq!(notification.priority, NotificationPriority::Low);
        assert!(auto_update_startup_notification_for_platform("windows", true).is_none());
        assert!(auto_update_startup_notification_for_platform("linux", false).is_none());
    }

    #[test]
    fn startup_notifications_promote_settings_errors_only() {
        let settings = SettingsJson::default();
        let errors = vec![validation_error("defaultShell")];

        let notifications = startup_notifications(&settings, &errors);

        // CC `main.tsx:4107-4110` seeds `notifications: { current: null, queue:
        // initialNotifications }` — the startup seed does NOT promote. The
        // first promote happens when the mounted tree runs `processQueue()`
        // (context/notifications.tsx:252), which is a separate transition.
        assert!(notifications.current.is_none());
        assert_eq!(
            notifications
                .queue
                .iter()
                .map(|n| n.key.as_str())
                .collect::<Vec<_>>(),
            vec![SETTINGS_ERRORS_NOTIFICATION_KEY]
        );
    }

    #[test]
    fn startup_notifications_are_empty_without_matching_producers() {
        let settings = SettingsJson::default();
        let notifications = startup_notifications(&settings, &[]);

        assert!(notifications.current.is_none());
        assert!(notifications.queue.is_empty());
    }

    #[test]
    fn startup_notification_gate_matches_official_remote_mode_and_once_guard() {
        let mut state = StartupNotificationState::default();
        let first = apply_startup_notifications_once(
            false,
            &mut state,
            [Notification::text(
                "first-startup",
                "first startup notification",
                NotificationPriority::Medium,
            )],
        );

        // Seed shape, not promoted state (CC `main.tsx:4107-4110`).
        assert!(first.current.is_none());
        assert_eq!(
            first
                .queue
                .iter()
                .map(|n| n.key.as_str())
                .collect::<Vec<_>>(),
            vec!["first-startup"]
        );
        assert!(state.has_run);

        let second = apply_startup_notifications_once(
            false,
            &mut state,
            [Notification::text(
                "second-startup",
                "second startup notification",
                NotificationPriority::Medium,
            )],
        );
        assert!(second.current.is_none());
        assert!(second.queue.is_empty());
    }

    #[test]
    fn startup_notification_gate_skips_remote_mode_without_setting_once_guard() {
        let mut state = StartupNotificationState::default();
        let remote = apply_startup_notifications_once(
            true,
            &mut state,
            [Notification::text(
                "remote-startup",
                "remote startup notification",
                NotificationPriority::Medium,
            )],
        );

        assert!(remote.current.is_none());
        assert!(remote.queue.is_empty());
        assert!(!state.has_run);
    }

    #[test]
    fn startup_notifications_with_gate_uses_settings_errors_as_compute_result() {
        let settings = SettingsJson::default();
        let errors = vec![validation_error("permissions")];
        let mut state = StartupNotificationState::default();

        let first = startup_notifications_with_gate(&settings, &errors, false, &mut state);
        // Seed shape (CC `main.tsx:4107-4110`): queued, not promoted.
        assert!(first.current.is_none());
        assert_eq!(
            first
                .queue
                .iter()
                .map(|n| n.key.as_str())
                .collect::<Vec<_>>(),
            vec![SETTINGS_ERRORS_NOTIFICATION_KEY]
        );

        let second = startup_notifications_with_gate(&settings, &errors, false, &mut state);
        assert!(second.current.is_none());
        assert!(second.queue.is_empty());
    }
}
