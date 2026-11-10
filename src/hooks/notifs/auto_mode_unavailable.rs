//! Maps to: CC `hooks/notifs/useAutoModeUnavailableNotification.ts` and
//! `utils/permissions/permissionSetup.ts#getAutoModeUnavailableNotification`.
//!
//! This is a pure main-screen notification seam. Nyan does not enable real
//! auto-mode classification; callers must pass already-known UI state and the
//! resolved official reason.

use crate::context::notifications::{Notification, NotificationColor, NotificationPriority};
use crate::types::permissions::PermissionMode;

pub const AUTO_MODE_UNAVAILABLE_NOTIFICATION_KEY: &str = "auto-mode-unavailable";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoModeUnavailableReason {
    Settings,
    CircuitBreaker,
    Model,
}

pub fn auto_mode_unavailable_text(reason: AutoModeUnavailableReason) -> &'static str {
    match reason {
        AutoModeUnavailableReason::Settings => "auto mode disabled by settings",
        AutoModeUnavailableReason::CircuitBreaker => "auto mode is unavailable for your plan",
        AutoModeUnavailableReason::Model => "auto mode unavailable for this model",
    }
}

pub fn should_show_auto_mode_unavailable_notification(
    current_mode: PermissionMode,
    previous_mode: PermissionMode,
    is_auto_mode_available: bool,
    has_auto_mode_opt_in: bool,
    transcript_classifier_enabled: bool,
    is_remote_mode: bool,
    already_shown: bool,
) -> bool {
    if !transcript_classifier_enabled || is_remote_mode || already_shown {
        return false;
    }

    current_mode == PermissionMode::Default
        && previous_mode != PermissionMode::Default
        && !is_auto_mode_available
        && has_auto_mode_opt_in
}

pub fn auto_mode_unavailable_notification(reason: AutoModeUnavailableReason) -> Notification {
    Notification::text(
        AUTO_MODE_UNAVAILABLE_NOTIFICATION_KEY,
        auto_mode_unavailable_text(reason),
        NotificationPriority::Medium,
    )
    .with_color(NotificationColor::Warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mode_unavailable_text_matches_official_reason_copy() {
        assert_eq!(
            auto_mode_unavailable_text(AutoModeUnavailableReason::Settings),
            "auto mode disabled by settings"
        );
        assert_eq!(
            auto_mode_unavailable_text(AutoModeUnavailableReason::CircuitBreaker),
            "auto mode is unavailable for your plan"
        );
        assert_eq!(
            auto_mode_unavailable_text(AutoModeUnavailableReason::Model),
            "auto mode unavailable for this model"
        );
    }

    #[test]
    fn auto_mode_unavailable_notification_matches_official_key_color_priority() {
        let notification =
            auto_mode_unavailable_notification(AutoModeUnavailableReason::CircuitBreaker);

        assert_eq!(notification.key, AUTO_MODE_UNAVAILABLE_NOTIFICATION_KEY);
        assert_eq!(notification.text, "auto mode is unavailable for your plan");
        assert_eq!(notification.color, Some(NotificationColor::Warning));
        assert_eq!(notification.priority, NotificationPriority::Medium);
    }

    #[test]
    fn auto_mode_unavailable_only_shows_when_carousel_wraps_past_auto_slot() {
        assert!(should_show_auto_mode_unavailable_notification(
            PermissionMode::Default,
            PermissionMode::Plan,
            false,
            true,
            true,
            false,
            false,
        ));
        assert!(!should_show_auto_mode_unavailable_notification(
            PermissionMode::Default,
            PermissionMode::Plan,
            true,
            true,
            true,
            false,
            false,
        ));
        assert!(!should_show_auto_mode_unavailable_notification(
            PermissionMode::AcceptEdits,
            PermissionMode::Plan,
            false,
            true,
            true,
            false,
            false,
        ));
        assert!(!should_show_auto_mode_unavailable_notification(
            PermissionMode::Default,
            PermissionMode::Default,
            false,
            true,
            true,
            false,
            false,
        ));
        assert!(!should_show_auto_mode_unavailable_notification(
            PermissionMode::Default,
            PermissionMode::Plan,
            false,
            false,
            true,
            false,
            false,
        ));
        assert!(!should_show_auto_mode_unavailable_notification(
            PermissionMode::Default,
            PermissionMode::Plan,
            false,
            true,
            false,
            false,
            false,
        ));
        assert!(!should_show_auto_mode_unavailable_notification(
            PermissionMode::Default,
            PermissionMode::Plan,
            false,
            true,
            true,
            true,
            false,
        ));
        assert!(!should_show_auto_mode_unavailable_notification(
            PermissionMode::Default,
            PermissionMode::Plan,
            false,
            true,
            true,
            false,
            true,
        ));
    }
}
