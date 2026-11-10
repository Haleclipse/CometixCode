//! Maps to: CC `components/permissions/SandboxPermissionRequest.tsx`.
//!
//! Ports the sandbox network-host permission dialog boundary. Runtime sandbox
//! enforcement and settings persistence stay in `utils/sandbox/*`; this
//! component only renders the official copy/options and emits the user response
//! shape consumed by the sandbox adapter.

use super::permission_dialog::PermissionDialog;
use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::utils::sandbox::sandbox_adapter::NetworkHostPattern;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SandboxPermissionResponse {
    pub allow: bool,
    pub persist_to_settings: bool,
}

#[derive(Default, Props)]
pub struct SandboxPermissionRequestProps<'a> {
    pub host_pattern: Option<NetworkHostPattern>,
    pub on_user_response: HandlerMut<'a, SandboxPermissionResponse>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxPermissionOptionValue {
    Yes,
    YesDontAskAgain,
    No,
}

impl SandboxPermissionOptionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::YesDontAskAgain => "yes-dont-ask-again",
            Self::No => "no",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxPermissionOption {
    pub label: String,
    pub value: SandboxPermissionOptionValue,
}

impl SandboxPermissionOption {
    fn select(label: impl Into<String>, value: SandboxPermissionOptionValue) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }

    pub fn to_select_option(&self) -> SelectOptionData {
        SelectOptionData {
            label: self.label.clone(),
            description: None,
            dim_description: true,
            value: self.value.as_str().to_string(),
            disabled: false,
            input: None,
        }
    }
}

/// Maps to: CC `SandboxPermissionRequest` `options` array. @nyan: the
/// persistent option is always offered (CC hides it under the managed
/// `allowManagedDomainsOnly`).
pub fn sandbox_permission_options(host: &str) -> Vec<SandboxPermissionOption> {
    vec![
        SandboxPermissionOption::select("Yes", SandboxPermissionOptionValue::Yes),
        SandboxPermissionOption::select(
            format!("Yes, and don't ask again for {host}"),
            SandboxPermissionOptionValue::YesDontAskAgain,
        ),
        SandboxPermissionOption::select(
            "No, and tell nyan what to do differently (esc)",
            SandboxPermissionOptionValue::No,
        ),
    ]
}

/// Maps to: CC `onSelect` switch response shape.
pub fn sandbox_permission_response_for_option(
    value: SandboxPermissionOptionValue,
) -> SandboxPermissionResponse {
    match value {
        SandboxPermissionOptionValue::Yes => SandboxPermissionResponse {
            allow: true,
            persist_to_settings: false,
        },
        SandboxPermissionOptionValue::YesDontAskAgain => SandboxPermissionResponse {
            allow: true,
            persist_to_settings: true,
        },
        SandboxPermissionOptionValue::No => SandboxPermissionResponse {
            allow: false,
            persist_to_settings: false,
        },
    }
}

/// Maps to: CC `SandboxPermissionRequest`.
#[component]
pub fn SandboxPermissionRequest<'a>(
    props: &mut SandboxPermissionRequestProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let host = props
        .host_pattern
        .as_ref()
        .map(|pattern| pattern.host.clone())
        .unwrap_or_default();
    let options = sandbox_permission_options(&host);
    let option_count = options.len().max(1);
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_select = hooks.use_state(|| Option::<SandboxPermissionOptionValue>::None);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_select = pending_select;
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
                        pending_select.set(Some(option.value));
                    }
                }
                KeyCode::Esc => {
                    pending_select.set(Some(SandboxPermissionOptionValue::No));
                }
                _ => {}
            }
        }
    });

    let selected = { *pending_select.read() };
    if let Some(value) = selected {
        pending_select.set(None);
        (props.on_user_response)(sandbox_permission_response_for_option(value));
    }

    let focused = focused_index.get().min(option_count - 1);
    let select_options = options
        .iter()
        .map(SandboxPermissionOption::to_select_option)
        .collect::<Vec<_>>();

    element! {
        PermissionDialog(title: "Network request outside of sandbox".to_string()) {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Host:".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    Text(content: format!(" {host}"), wrap: TextWrap::NoWrap)
                }
                View(margin_top: 1u32) {
                    Text(content: "Do you want to allow this connection?".to_string(), wrap: TextWrap::Wrap)
                }
                View {
                    Select(
                        options: select_options,
                        focused_index: focused,
                        visible_option_count: option_count,
                        layout: SelectLayout::Compact,
                        hide_indexes: true,
                    )
                }
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

    fn render_sandbox(host: &str) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SandboxPermissionRequest(
                    host_pattern: Some(NetworkHostPattern::new(host)),
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn sandbox_permission_options_match_official_labels() {
        let options = sandbox_permission_options("api.example.com");
        assert_eq!(options[0].label, "Yes");
        assert_eq!(
            options[1].label,
            "Yes, and don't ask again for api.example.com"
        );
        assert_eq!(
            options[2].label,
            "No, and tell nyan what to do differently (esc)"
        );
        assert_eq!(options.len(), 3);
    }

    #[test]
    fn sandbox_permission_response_shape_matches_official_switch() {
        assert_eq!(
            sandbox_permission_response_for_option(SandboxPermissionOptionValue::Yes),
            SandboxPermissionResponse {
                allow: true,
                persist_to_settings: false,
            }
        );
        assert_eq!(
            sandbox_permission_response_for_option(SandboxPermissionOptionValue::YesDontAskAgain),
            SandboxPermissionResponse {
                allow: true,
                persist_to_settings: true,
            }
        );
        assert_eq!(
            sandbox_permission_response_for_option(SandboxPermissionOptionValue::No),
            SandboxPermissionResponse {
                allow: false,
                persist_to_settings: false,
            }
        );
    }

    #[test]
    fn sandbox_permission_request_renders_official_copy_and_options() {
        let text = render_sandbox("api.example.com");
        assert!(
            text.contains("Network request outside of sandbox"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Host:"), "canvas=\n{text}");
        assert!(text.contains("api.example.com"), "canvas=\n{text}");
        assert!(
            text.contains("Do you want to allow this connection?"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Yes"), "canvas=\n{text}");
        assert!(
            text.contains("Yes, and don't ask again for api.example.com"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("No, and tell nyan what to do differently"),
            "canvas=\n{text}"
        );
    }

    #[tokio::test]
    async fn sandbox_permission_request_escape_emits_official_cancel_response() {
        let responses = Arc::new(Mutex::new(Vec::new()));
        let responses_clone = Arc::clone(&responses);
        let mut app = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SandboxPermissionRequest(
                    host_pattern: Some(NetworkHostPattern::new("api.example.com")),
                    on_user_response: move |response| {
                        responses_clone.lock().unwrap().push(response);
                    },
                )
            }
        };
        let mut render_loop = Box::pin(
            app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Esc)]))
                    .with_size(120, 30),
            ),
        );
        for _ in 0..10 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                None
            })
            .await;
            if next.is_none() {
                break;
            }
        }

        let responses = responses.lock().unwrap().clone();
        assert_eq!(
            responses,
            vec![SandboxPermissionResponse {
                allow: false,
                persist_to_settings: false,
            }]
        );
    }
}
