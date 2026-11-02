//! Maps to: CC `hooks/notifs/useLspInitializationNotification.tsx`.
//!
//! Nyan keeps this as a pure notification seam. It models the official
//! visible payload for already-known LSP manager/server errors, but it does not
//! start or poll LSP servers, mutate plugin state, or write settings/session
//! data.

use crate::context::notifications::{
    Notification, NotificationColor, NotificationPriority, NotificationSegment,
};
use crate::services::lsp::manager::InitializationStatus;
use crate::services::lsp::server_manager::LspServerSnapshot;
use crate::services::lsp::types::LspServerState;
use indexmap::IndexMap;
use std::collections::HashSet;

/// Maps to: CC `hooks/notifs/useLspInitializationNotification.tsx#LSP_POLL_INTERVAL_MS`.
pub const LSP_POLL_INTERVAL_MS: u64 = 5_000;
pub const LSP_NOTIFICATION_TIMEOUT_MS: u64 = 8_000;

pub fn lsp_error_key(source: &str) -> String {
    format!("lsp-error-{source}")
}

pub fn lsp_error_signature(source: &str, error_message: &str) -> String {
    format!("{source}:{error_message}")
}

pub fn lsp_error_display_name(source: &str) -> String {
    if let Some(rest) = source.strip_prefix("plugin:") {
        return rest.split(':').next().unwrap_or(source).to_string();
    }
    source.to_string()
}

pub fn lsp_error_notification(source: &str, _error_message: &str) -> Notification {
    let display_name = lsp_error_display_name(source);
    let heading = format!("LSP for {display_name} failed");
    let text = format!("{heading} · /plugin for details");

    Notification::text(lsp_error_key(source), text, NotificationPriority::Medium)
        .with_segments(vec![
            NotificationSegment::text(heading).with_color(NotificationColor::Error),
            NotificationSegment::text(" · /plugin for details").with_dim(true),
        ])
        .with_timeout_ms(LSP_NOTIFICATION_TIMEOUT_MS)
}

pub fn lsp_error_notification_once(
    notified_errors: &mut HashSet<String>,
    source: &str,
    error_message: &str,
) -> Option<Notification> {
    let signature = lsp_error_signature(source, error_message);
    if !notified_errors.insert(signature) {
        return None;
    }
    Some(lsp_error_notification(source, error_message))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LspInitializationPollOutcome {
    pub notifications: Vec<Notification>,
    pub should_continue_polling: bool,
}

/// Maps to: CC `useLspInitializationNotification.tsx#poll`.
pub fn lsp_initialization_poll_outcome_from_status(
    notified_errors: &mut HashSet<String>,
    status: InitializationStatus,
    servers: Option<&IndexMap<String, LspServerSnapshot>>,
) -> LspInitializationPollOutcome {
    match status {
        InitializationStatus::Failed { error } => {
            let notifications = lsp_error_notification_once(notified_errors, "lsp-manager", &error)
                .into_iter()
                .collect();
            LspInitializationPollOutcome {
                notifications,
                should_continue_polling: false,
            }
        }
        InitializationStatus::Pending | InitializationStatus::NotStarted => {
            LspInitializationPollOutcome {
                notifications: Vec::new(),
                should_continue_polling: true,
            }
        }
        InitializationStatus::Success => {
            let mut notifications = Vec::new();
            if let Some(servers) = servers {
                for (server_name, server) in servers {
                    if server.state == LspServerState::Error
                        && let Some(error) = server.last_error.as_deref()
                        && let Some(notification) =
                            lsp_error_notification_once(notified_errors, server_name, error)
                    {
                        notifications.push(notification);
                    }
                }
            }
            LspInitializationPollOutcome {
                notifications,
                should_continue_polling: true,
            }
        }
    }
}

/// Maps to: CC `useLspInitializationNotification()`
/// (`hooks/notifs/useLspInitializationNotification.tsx`), whose body is
/// `useInterval(poll, shouldPoll ? LSP_POLL_INTERVAL_MS : null)` at `:134`.
/// CC calls it from `screens/REPL.tsx`, so the polling starts at REPL mount.
///
/// Nyan previously ran the same loop from a `tokio::spawn` in `main.rs`
/// issued before `render_loop()`, which raced `AppStateProvider`'s
/// `bind_on_change`: an LSP notification landing first was silent where CC's
/// notifies. Owning it here, called from `Repl`, makes the ordering structural
/// (P5 G10).
///
/// Deviation, deliberate: CC uses `useInterval` with a `null` period to stop
/// polling; iocraft has no equivalent, so the loop exits on
/// `should_continue_polling == false` — same terminal condition, expressed
/// where the loop lives.
pub fn use_lsp_initialization_notification(
    hooks: &mut iocraft::prelude::Hooks,
    app_store: Option<crate::state::store::AppStore>,
) {
    use iocraft::prelude::UseFuture;

    hooks.use_future(async move {
        // CC gates the interval on `shouldPoll` (`:134`), whose inputs
        // (bootstrap remote-mode, scroll-drain) Nyan does not expose yet —
        // see `poll_lsp_initialization_notifications`. Until it does, the
        // build gate that guarded this loop while it lived in `main.rs` is
        // preserved: component harnesses must not start a real LSP poll, which
        // would write notifications into their store mid-assertion.
        if cfg!(test) {
            return;
        }
        let Some(store) = app_store else {
            return;
        };
        let mut notified = HashSet::<String>::new();
        let mut notifications = crate::context::notifications::NotificationsWriter::new(store);
        loop {
            let outcome = poll_lsp_initialization_notifications(&mut notified);
            for notification in outcome.notifications {
                notifications.add_notification(notification);
            }
            if !outcome.should_continue_polling {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(LSP_POLL_INTERVAL_MS)).await;
        }
    });
}

pub fn poll_lsp_initialization_notifications(
    notified_errors: &mut HashSet<String>,
) -> LspInitializationPollOutcome {
    // Maps to: CC `getInitializationStatus()` + `getLspServerManager().getAllServers()`.
    // Nyan does not currently expose the bootstrap remote-mode or scroll-drain
    // flags used as fast skip guards by the React hook, so polling remains cheap
    // and read-only here.
    let status = crate::services::lsp::manager::get_initialization_status();
    let servers = crate::services::lsp::manager::get_all_servers_snapshot();
    lsp_initialization_poll_outcome_from_status(notified_errors, status, servers.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_error_notification_matches_official_payload_for_plugin_source() {
        let notification =
            lsp_error_notification("plugin:typescript-lsp:typescript", "server crashed");

        assert_eq!(
            notification.key,
            "lsp-error-plugin:typescript-lsp:typescript"
        );
        assert_eq!(
            notification.text,
            "LSP for typescript-lsp failed · /plugin for details"
        );
        assert_eq!(notification.priority, NotificationPriority::Medium);
        assert_eq!(notification.timeout_ms, Some(LSP_NOTIFICATION_TIMEOUT_MS));
        assert_eq!(notification.color, None);
        assert_eq!(notification.segments.len(), 2);
        assert_eq!(
            notification.segments[0].text,
            "LSP for typescript-lsp failed"
        );
        assert_eq!(
            notification.segments[0].color,
            Some(NotificationColor::Error)
        );
        assert_eq!(notification.segments[1].text, " · /plugin for details");
        assert!(notification.segments[1].dim);
    }

    #[test]
    fn lsp_error_notification_matches_official_payload_for_manager_source() {
        let notification = lsp_error_notification("lsp-manager", "init failed");

        assert_eq!(notification.key, "lsp-error-lsp-manager");
        assert_eq!(
            notification.text,
            "LSP for lsp-manager failed · /plugin for details"
        );
    }

    #[test]
    fn lsp_error_notification_once_suppresses_duplicate_source_error_pairs() {
        let mut notified = HashSet::new();

        assert!(lsp_error_notification_once(&mut notified, "rust", "boom").is_some());
        assert!(lsp_error_notification_once(&mut notified, "rust", "boom").is_none());
        assert!(lsp_error_notification_once(&mut notified, "rust", "different").is_some());
        assert!(lsp_error_notification_once(&mut notified, "python", "boom").is_some());
    }

    /// Dedup/stop semantics of the poll outcome, in isolation. The status value
    /// is constructed here, which proves nothing about reachability — that half
    /// lives in
    /// `services::lsp::manager::tests::failed_initialization_clears_the_instance_and_drives_the_official_poll`,
    /// which drives a real `pending → failed` transition through
    /// `initializeLspServerManager` and calls
    /// `poll_lsp_initialization_notifications` on the resulting global state.
    /// Before that test existed this one was a false negative: green while the
    /// production path could not produce its input.
    #[test]
    fn lsp_poll_outcome_matches_official_manager_failed_stop_and_dedup() {
        let mut notified = HashSet::new();
        let outcome = lsp_initialization_poll_outcome_from_status(
            &mut notified,
            InitializationStatus::Failed {
                error: "init failed".to_string(),
            },
            None,
        );
        assert_eq!(outcome.notifications.len(), 1);
        assert!(!outcome.should_continue_polling);
        assert_eq!(outcome.notifications[0].key, "lsp-error-lsp-manager");

        let duplicate = lsp_initialization_poll_outcome_from_status(
            &mut notified,
            InitializationStatus::Failed {
                error: "init failed".to_string(),
            },
            None,
        );
        assert!(duplicate.notifications.is_empty());
        assert!(!duplicate.should_continue_polling);
    }

    #[test]
    fn lsp_poll_outcome_reports_server_errors_after_success() {
        let mut servers = IndexMap::new();
        servers.insert(
            "plugin:typescript-lsp:typescript".to_string(),
            LspServerSnapshot {
                name: "plugin:typescript-lsp:typescript".to_string(),
                state: LspServerState::Error,
                last_error: Some("server crashed".to_string()),
            },
        );
        servers.insert(
            "rust".to_string(),
            LspServerSnapshot {
                name: "rust".to_string(),
                state: LspServerState::Running,
                last_error: None,
            },
        );
        let mut notified = HashSet::new();
        let outcome = lsp_initialization_poll_outcome_from_status(
            &mut notified,
            InitializationStatus::Success,
            Some(&servers),
        );
        assert!(outcome.should_continue_polling);
        assert_eq!(outcome.notifications.len(), 1);
        assert_eq!(
            outcome.notifications[0].text,
            "LSP for typescript-lsp failed · /plugin for details"
        );
    }
}
