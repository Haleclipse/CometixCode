//! Maps to: CC `hooks/notifs/useSettingsErrors.tsx`.
//!
//! Nyan keeps this as a pure notification producer plus a startup wiring
//! seam. It only reports already-loaded settings validation errors and never
//! writes settings, sessions, or filesystem state.

#[cfg(test)]
use crate::context::notifications::NotificationsState;
use crate::context::notifications::{Notification, NotificationColor, NotificationPriority};
use crate::utils::settings::ValidationError;

pub const SETTINGS_ERRORS_NOTIFICATION_KEY: &str = "settings-errors";
pub const SETTINGS_ERRORS_NOTIFICATION_TIMEOUT_MS: u64 = 60_000;

pub fn settings_errors_notification(error_count: usize) -> Option<Notification> {
    if error_count == 0 {
        return None;
    }

    let issue_word = if error_count == 1 { "issue" } else { "issues" };
    Some(
        Notification::text(
            SETTINGS_ERRORS_NOTIFICATION_KEY,
            format!("Found {error_count} settings {issue_word} · /doctor for details"),
            NotificationPriority::High,
        )
        .with_color(NotificationColor::Warning)
        .with_timeout_ms(SETTINGS_ERRORS_NOTIFICATION_TIMEOUT_MS),
    )
}

pub fn settings_errors_notification_from_errors(
    errors: &[ValidationError],
) -> Option<Notification> {
    settings_errors_notification(errors.len())
}

#[cfg(test)]
pub fn notifications_with_settings_errors(errors: &[ValidationError]) -> NotificationsState {
    let mut notifications = NotificationsState::default();
    if let Some(notification) = settings_errors_notification_from_errors(errors) {
        notifications.queue.push(notification);
    }
    notifications
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validation_error(path: &str) -> ValidationError {
        ValidationError {
            file: Some("settings.json".to_string()),
            path: path.to_string(),
            message: "invalid setting".to_string(),
            expected: None,
            invalid_value: None,
            suggestion: None,
        }
    }

    #[test]
    fn settings_errors_notification_matches_official_text_priority_and_timeout() {
        let single = settings_errors_notification(1).expect("one error should notify");
        assert_eq!(single.key, SETTINGS_ERRORS_NOTIFICATION_KEY);
        assert_eq!(single.text, "Found 1 settings issue · /doctor for details");
        assert_eq!(single.color, Some(NotificationColor::Warning));
        assert_eq!(single.priority, NotificationPriority::High);
        assert_eq!(
            single.timeout_ms,
            Some(SETTINGS_ERRORS_NOTIFICATION_TIMEOUT_MS)
        );

        let plural = settings_errors_notification(2).expect("two errors should notify");
        assert_eq!(plural.text, "Found 2 settings issues · /doctor for details");
    }

    #[test]
    fn settings_errors_notification_is_suppressed_without_errors() {
        assert!(settings_errors_notification(0).is_none());
        assert!(settings_errors_notification_from_errors(&[]).is_none());
    }

    #[test]
    fn settings_errors_notification_counts_loaded_validation_errors() {
        let errors = vec![validation_error("model"), validation_error("defaultShell")];
        let notification =
            settings_errors_notification_from_errors(&errors).expect("errors should notify");

        assert_eq!(
            notification.text,
            "Found 2 settings issues · /doctor for details"
        );
    }

    #[test]
    fn settings_errors_startup_state_queues_warning_row() {
        let errors = vec![validation_error("model")];
        let notifications = notifications_with_settings_errors(&errors);

        // CC `main.tsx:4107-4110` — the startup seed is `{ current: null,
        // queue: [...] }`; promotion is the mounted tree's `processQueue()`.
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
}
