//! Maps to: CC `components/RemoteCallout.tsx`.
//!
//! Safety boundary: official `RemoteCallout` reads global config, bridge
//! entitlement, and Claude.ai OAuth tokens; on mount it writes
//! `remoteDialogSeen: true`. Nyan keeps `should_show_remote_callout` pure
//! through an explicit snapshot and emits callback results only. It does not
//! read auth/config, write settings, open bridge transports, or start network
//! connections.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::permissions::permission_dialog::PermissionDialog;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteCalloutSnapshot {
    /// Maps to `getGlobalConfig().remoteDialogSeen`.
    pub remote_dialog_seen: bool,
    /// Maps to `isBridgeEnabled()`.
    pub bridge_enabled: bool,
    /// Maps to `getClaudeAIOAuthTokens()?.accessToken` presence.
    pub has_claude_ai_access_token: bool,
}

/// Maps to CC `shouldShowRemoteCallout()`.
pub fn should_show_remote_callout(snapshot: &RemoteCalloutSnapshot) -> bool {
    if snapshot.remote_dialog_seen {
        return false;
    }
    if !snapshot.bridge_enabled {
        return false;
    }
    if !snapshot.has_claude_ai_access_token {
        return false;
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteCalloutSelection {
    Enable,
    Dismiss,
}

impl RemoteCalloutSelection {
    fn value(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Dismiss => "dismiss",
        }
    }
}

fn selection_from_value(value: &str) -> RemoteCalloutSelection {
    match value {
        "enable" => RemoteCalloutSelection::Enable,
        _ => RemoteCalloutSelection::Dismiss,
    }
}

/// Maps to CC `RemoteCallout` `options`.
pub fn remote_callout_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Enable Remote Control for this session".to_string(),
            description: Some("Opens a secure connection to claude.ai.".to_string()),
            value: RemoteCalloutSelection::Enable.value().to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Never mind".to_string(),
            description: Some("You can always enable it later with /remote-control.".to_string()),
            value: RemoteCalloutSelection::Dismiss.value().to_string(),
            ..SelectOptionData::default()
        },
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCalloutDone {
    pub selection: RemoteCalloutSelection,
    /// True because official `useEffect` would mark `remoteDialogSeen` on mount.
    pub would_mark_seen: bool,
}

#[derive(Default, Props)]
pub struct RemoteCalloutProps<'a> {
    pub on_done: HandlerMut<'a, RemoteCalloutDone>,
}

/// Maps to CC `RemoteCallout`.
#[component]
pub fn RemoteCallout<'a>(
    props: &mut RemoteCalloutProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_selection = hooks.use_state(|| Option::<RemoteCalloutSelection>::None);
    let options = remote_callout_options();
    let option_count = options.len().max(1);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_selection = pending_selection;
        let options = options.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    focused_index.set(focused_index.get().saturating_sub(1));
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    focused_index
                        .set((focused_index.get() + 1).min(options.len().saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(option) = options.get(focused_index.get()) {
                        pending_selection.set(Some(selection_from_value(&option.value)));
                    }
                }
                KeyCode::Esc => {
                    pending_selection.set(Some(RemoteCalloutSelection::Dismiss));
                }
                _ => {}
            }
        }
    });

    let selected = { *pending_selection.read() };
    if let Some(selection) = selected {
        pending_selection.set(None);
        (props.on_done)(RemoteCalloutDone {
            selection,
            would_mark_seen: true,
        });
    }

    element! {
        PermissionDialog(title: "Remote Control".to_string()) {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                View(margin_bottom: 1u32, flex_direction: FlexDirection::Column) {
                    Text(
                        content: "Remote Control lets you access this CLI session from the web (claude.ai/code) or the Claude app, so you can pick up where you left off on any device.".to_string(),
                        wrap: TextWrap::Wrap,
                    )
                    Text(content: " ".to_string())
                    Text(
                        content: "You can disconnect remote access anytime by running /remote-control again.".to_string(),
                        wrap: TextWrap::Wrap,
                    )
                }
                Select(
                    options: options,
                    focused_index: focused_index.get().min(option_count - 1),
                    visible_option_count: option_count,
                    layout: SelectLayout::Compact,
                    hide_indexes: true,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn render_callout() -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RemoteCallout
            }
        }
        .render(Some(120))
        .to_string()
    }

    async fn drive(
        events: Vec<TerminalEvent>,
        results: Arc<Mutex<Vec<RemoteCalloutDone>>>,
    ) -> String {
        let results_for_handler = results.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                RemoteCallout(
                    on_done: move |done: RemoteCalloutDone| results_for_handler.lock().unwrap().push(done),
                )
            }
        };
        let mut render_loop = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(stream::iter(events)).with_size(120, 24),
        ));
        let mut last = String::new();
        for _ in 0..6 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                None
            })
            .await;
            let Some(canvas) = next else {
                break;
            };
            last = canvas.to_string();
        }
        last
    }

    #[test]
    fn remote_callout_gate_matches_official_conditions() {
        let shown = RemoteCalloutSnapshot {
            remote_dialog_seen: false,
            bridge_enabled: true,
            has_claude_ai_access_token: true,
        };
        assert!(should_show_remote_callout(&shown));
        assert!(!should_show_remote_callout(&RemoteCalloutSnapshot {
            remote_dialog_seen: true,
            ..shown.clone()
        }));
        assert!(!should_show_remote_callout(&RemoteCalloutSnapshot {
            bridge_enabled: false,
            ..shown.clone()
        }));
        assert!(!should_show_remote_callout(&RemoteCalloutSnapshot {
            has_claude_ai_access_token: false,
            ..shown
        }));
    }

    #[test]
    fn remote_callout_renders_official_copy_and_options() {
        let text = render_callout();
        assert!(text.contains("Remote Control"), "canvas=\n{text}");
        assert!(text.contains("claude.ai/code"), "canvas=\n{text}");
        assert!(
            text.contains("Enable Remote Control for this session"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Opens a secure connection"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Never mind"), "canvas=\n{text}");
        assert!(
            !text.contains("Enter to confirm · Esc to cancel"),
            "official RemoteCallout uses PermissionDialog, not design-system Dialog input guide; canvas=\n{text}"
        );
    }

    #[test]
    fn remote_callout_enter_and_cancel_emit_safe_done_shapes() {
        let results = Arc::new(Mutex::new(Vec::<RemoteCalloutDone>::new()));
        futures::executor::block_on(drive(vec![key(KeyCode::Enter)], results.clone()));
        futures::executor::block_on(drive(
            vec![key(KeyCode::Down), key(KeyCode::Enter)],
            results.clone(),
        ));
        futures::executor::block_on(drive(vec![key(KeyCode::Esc)], results.clone()));

        assert_eq!(
            *results.lock().unwrap(),
            vec![
                RemoteCalloutDone {
                    selection: RemoteCalloutSelection::Enable,
                    would_mark_seen: true,
                },
                RemoteCalloutDone {
                    selection: RemoteCalloutSelection::Dismiss,
                    would_mark_seen: true,
                },
                RemoteCalloutDone {
                    selection: RemoteCalloutSelection::Dismiss,
                    would_mark_seen: true,
                },
            ]
        );
    }
}
