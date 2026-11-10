//! Maps to: CC `hooks/notifs/useDeprecationWarningNotification.tsx`.
//!
//! Nyan keeps model deprecation lookup outside this main-screen notification
//! seam. Callers may pass an already-known warning string; this helper only
//! mirrors the official notification payload and duplicate-suppression state.

use crate::context::notifications::{Notification, NotificationColor, NotificationPriority};

pub const MODEL_DEPRECATION_WARNING_KEY: &str = "model-deprecation-warning";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeprecationWarningNotificationState {
    pub last_warning: Option<String>,
}

pub fn deprecation_warning_notification(
    deprecation_warning: Option<&str>,
    state: &mut DeprecationWarningNotificationState,
) -> Option<Notification> {
    let Some(warning) = deprecation_warning.filter(|warning| !warning.trim().is_empty()) else {
        state.last_warning = None;
        return None;
    };

    if state.last_warning.as_deref() == Some(warning) {
        return None;
    }

    state.last_warning = Some(warning.to_string());
    Some(
        Notification::text(
            MODEL_DEPRECATION_WARNING_KEY,
            warning,
            NotificationPriority::High,
        )
        .with_color(NotificationColor::Warning),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deprecation_warning_notification_matches_official_payload() {
        let mut state = DeprecationWarningNotificationState::default();
        let notification = deprecation_warning_notification(
            Some("⚠ Claude 3 Opus will be retired on January 5, 2026. Consider switching to a newer model."),
            &mut state,
        )
        .expect("deprecated model warning should show once");

        assert_eq!(notification.key, MODEL_DEPRECATION_WARNING_KEY);
        assert_eq!(
            notification.text,
            "⚠ Claude 3 Opus will be retired on January 5, 2026. Consider switching to a newer model."
        );
        assert_eq!(notification.color, Some(NotificationColor::Warning));
        assert_eq!(notification.priority, NotificationPriority::High);
        assert_eq!(
            state.last_warning.as_deref(),
            Some(
                "⚠ Claude 3 Opus will be retired on January 5, 2026. Consider switching to a newer model."
            )
        );
    }

    #[test]
    fn deprecation_warning_notification_suppresses_duplicates_until_reset() {
        let mut state = DeprecationWarningNotificationState::default();
        let warning = "⚠ Claude 3.7 Sonnet will be retired on February 19, 2026. Consider switching to a newer model.";

        assert!(deprecation_warning_notification(Some(warning), &mut state).is_some());
        assert!(deprecation_warning_notification(Some(warning), &mut state).is_none());

        assert!(deprecation_warning_notification(None, &mut state).is_none());
        assert_eq!(state.last_warning, None);
        assert!(deprecation_warning_notification(Some(warning), &mut state).is_some());
    }
}
