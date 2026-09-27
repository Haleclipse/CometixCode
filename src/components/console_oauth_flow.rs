//! Maps to: CC `components/ConsoleOAuthFlow.tsx`:1-636.
//!
//! Official ConsoleOAuthFlow owns OAuth service lifecycle, browser launch,
//! clipboard writes, token installation, notifications, analytics, and keychain
//! writes. Cometix keeps this slice render-only: status is provided as props and
//! no external OAuth/login side effects are executed.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use crate::components::spinner::Spinner;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

pub(crate) const PASTE_HERE_MSG: &str = "Paste code here if prompted > ";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ConsoleOAuthMode {
    #[default]
    Login,
    SetupToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForceLoginMethod {
    ClaudeAi,
    Console,
}

pub(crate) fn forced_method_message(method: Option<ForceLoginMethod>) -> Option<&'static str> {
    match method {
        Some(ForceLoginMethod::ClaudeAi) => {
            Some("Login method pre-selected: Subscription Plan (Claude Pro/Max)")
        }
        Some(ForceLoginMethod::Console) => {
            Some("Login method pre-selected: API Usage Billing (Anthropic Console)")
        }
        None => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OAuthStatus {
    Idle,
    PlatformSetup,
    ReadyToStart,
    WaitingForLogin {
        url: String,
    },
    CreatingApiKey,
    AboutToRetry,
    Success {
        token: Option<String>,
        email: Option<String>,
    },
    Error {
        message: String,
        can_retry: bool,
    },
}

impl Default for OAuthStatus {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Default, Props)]
pub(crate) struct ConsoleOAuthFlowProps {
    pub oauth_status: OAuthStatus,
    pub mode: ConsoleOAuthMode,
    pub starting_message: Option<String>,
    pub force_login_method: Option<ForceLoginMethod>,
    pub show_paste_prompt: bool,
    pub url_copied: bool,
    pub pasted_code: String,
    pub cursor_offset: usize,
    pub text_input_columns: usize,
}

fn masked_code(value: &str) -> String {
    "*".repeat(value.chars().count())
}

fn login_method_options() -> Vec<SelectOptionData> {
    vec![
        SelectOptionData {
            label: "Claude account with subscription · Pro, Max, Team, or Enterprise\n".to_string(),
            value: "claudeai".to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "Anthropic Console account · API usage billing\n".to_string(),
            value: "console".to_string(),
            ..SelectOptionData::default()
        },
        SelectOptionData {
            label: "3rd-party platform · Amazon Bedrock, Microsoft Foundry, or Vertex AI\n"
                .to_string(),
            value: "platform".to_string(),
            ..SelectOptionData::default()
        },
    ]
}

/// Maps to: CC `components/ConsoleOAuthFlow.tsx`:325-382 render tree.
#[component]
pub(crate) fn ConsoleOAuthFlow(
    props: &ConsoleOAuthFlowProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let forced_method_message = forced_method_message(props.force_login_method).map(str::to_string);
    let oauth_status = props.oauth_status.clone();
    let mode = props.mode;
    let show_paste_prompt = props.show_paste_prompt;
    let url_copied = props.url_copied;
    let starting_message = props.starting_message.clone();
    let pasted_code = props.pasted_code.clone();
    let cursor_offset = props.cursor_offset;
    let text_input_columns = props.text_input_columns;

    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            #(if let OAuthStatus::WaitingForLogin { url } = &oauth_status {
                if show_paste_prompt {
                    Some(render_url_to_copy(url, url_copied, theme))
                } else {
                    None
                }
            } else {
                None
            })
            #(if let OAuthStatus::Success { token: Some(token), .. } = &oauth_status {
                if mode == ConsoleOAuthMode::SetupToken {
                    Some(render_setup_token_success(token.clone(), theme))
                } else {
                    None
                }
            } else {
                None
            })
            View(padding_left: 1u32, flex_direction: FlexDirection::Column, row_gap: 1u32) {
                OAuthStatusMessage(
                    oauth_status: oauth_status,
                    mode: mode,
                    starting_message: starting_message,
                    forced_method_message: forced_method_message,
                    show_paste_prompt: show_paste_prompt,
                    pasted_code: pasted_code,
                    cursor_offset: cursor_offset,
                    text_input_columns: text_input_columns,
                )
            }
        }
    }
}

fn render_url_to_copy(url: &str, url_copied: bool, theme: Theme) -> AnyElement<'static> {
    let url = url.to_string();
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32, padding_bottom: 1u32) {
            View(padding_left: 1u32, padding_right: 1u32, flex_direction: FlexDirection::Row) {
                Text(content: "Browser didn't open? Use the url below to sign in ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                #(if url_copied {
                    element! { Text(content: "(Copied!)".to_string(), color: theme.success, wrap: TextWrap::NoWrap) }.into_any()
                } else {
                    element! {
                        View(flex_direction: FlexDirection::Row) {
                            Text(content: "(".to_string(), dim: true, wrap: TextWrap::NoWrap)
                            KeyboardShortcutHint(shortcut: "c".to_string(), action: "copy".to_string())
                            Text(content: ")".to_string(), dim: true, wrap: TextWrap::NoWrap)
                        }
                    }.into_any()
                })
            }
            Text(content: url, dim: true, wrap: TextWrap::Wrap)
        }
    }
    .into_any()
}

fn render_setup_token_success(token: String, theme: Theme) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32, padding_top: 1u32) {
            Text(content: "✓ Long-lived authentication token created successfully!".to_string(), color: theme.success)
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                Text(content: "Your OAuth token (valid for 1 year):".to_string())
                Text(content: token, color: theme.warning)
                Text(content: "Store this token securely. You won't be able to see it again.".to_string(), dim: true)
                Text(content: "Use this token by setting: export CLAUDE_CODE_OAUTH_TOKEN=<token>".to_string(), dim: true)
            }
        }
    }
    .into_any()
}

#[derive(Default, Props)]
pub(crate) struct OAuthStatusMessageProps {
    pub oauth_status: OAuthStatus,
    pub mode: ConsoleOAuthMode,
    pub starting_message: Option<String>,
    pub forced_method_message: Option<String>,
    pub show_paste_prompt: bool,
    pub pasted_code: String,
    pub cursor_offset: usize,
    pub text_input_columns: usize,
}

/// Maps to: CC `components/ConsoleOAuthFlow.tsx`:408-636 `OAuthStatusMessage(...)`.
#[component]
pub(crate) fn OAuthStatusMessage(
    props: &OAuthStatusMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    match &props.oauth_status {
        OAuthStatus::Idle => render_idle(props.starting_message.clone(), theme),
        OAuthStatus::PlatformSetup => render_platform_setup(theme),
        OAuthStatus::ReadyToStart => render_opening_browser(),
        OAuthStatus::WaitingForLogin { .. } => render_waiting_for_login(
            props.forced_method_message.clone(),
            props.show_paste_prompt,
            &props.pasted_code,
            props.cursor_offset,
            props.text_input_columns,
        ),
        OAuthStatus::CreatingApiKey => render_creating_api_key(),
        OAuthStatus::AboutToRetry => element! {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                Text(content: "Retrying…".to_string(), color: theme.permission)
            }
        }
        .into_any(),
        OAuthStatus::Success { token, email } => render_success(props.mode, token, email, theme),
        OAuthStatus::Error { message, can_retry } => render_error(message, *can_retry, theme),
    }
}

fn render_idle(starting_message: Option<String>, theme: Theme) -> AnyElement<'static> {
    // CC :434-475 Text labels: child inactive hints and explicit final newline.
    let mut option_labels = BTreeMap::new();
    for (value, prefix, hint) in [
        (
            "claudeai",
            "Claude account with subscription · ",
            "Pro, Max, Team, or Enterprise",
        ),
        (
            "console",
            "Anthropic Console account · ",
            "API usage billing",
        ),
        (
            "platform",
            "3rd-party platform · ",
            "Amazon Bedrock, Microsoft Foundry, or Vertex AI",
        ),
    ] {
        let mut hint_segment = StyledSegment::new(hint);
        hint_segment.styles.color = Some(theme.inactive);
        option_labels.insert(
            value.to_string(),
            vec![
                StyledSegment::new(prefix),
                hint_segment,
                StyledSegment::new("\n"),
            ],
        );
    }
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32, margin_top: 1u32) {
            Text(
                content: starting_message.unwrap_or_else(|| "Claude Code can be used with your Claude subscription or billed based on API usage through your Console account.".to_string()),
                weight: Weight::Bold,
                wrap: TextWrap::Wrap,
            )
            Text(content: "Select login method:".to_string())
            View {
                Select(
                    is_disabled: false,
                    hide_indexes: false,
                    visible_option_count: 3usize,
                    options: login_method_options(),
                    option_labels,
                    focused_index: 0usize,
                    selected_value: None,
                    visible_from_index: 0usize,
                    layout: SelectLayout::Compact,
                )
            }
        }
    }
    .into_any()
}

fn render_platform_setup(theme: Theme) -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32, margin_top: 1u32) {
            Text(content: "Using 3rd-party platforms".to_string(), weight: Weight::Bold)
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                Text(content: "Claude Code supports Amazon Bedrock, Microsoft Foundry, and Vertex AI. Set the required environment variables, then restart Claude Code.".to_string(), wrap: TextWrap::Wrap)
                Text(content: "If you are part of an enterprise organization, contact your administrator for setup instructions.".to_string(), wrap: TextWrap::Wrap)
                View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                    Text(content: "Documentation:".to_string(), weight: Weight::Bold)
                    Text(content: "· Amazon Bedrock: https://code.claude.com/docs/en/amazon-bedrock".to_string(), wrap: TextWrap::Wrap)
                    Text(content: "· Microsoft Foundry: https://code.claude.com/docs/en/microsoft-foundry".to_string(), wrap: TextWrap::Wrap)
                    Text(content: "· Vertex AI: https://code.claude.com/docs/en/google-vertex-ai".to_string(), wrap: TextWrap::Wrap)
                }
                View(margin_top: 1u32) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Press ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                        Text(content: "Enter".to_string(), color: theme.inactive, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: " to go back to login options.".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                    }
                }
            }
        }
    }
    .into_any()
}

fn render_opening_browser() -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            View(flex_direction: FlexDirection::Row) {
                Spinner
                Text(content: "Opening browser to sign in…".to_string())
            }
        }
    }
    .into_any()
}

fn render_waiting_for_login(
    forced_method_message: Option<String>,
    show_paste_prompt: bool,
    pasted_code: &str,
    _cursor_offset: usize,
    text_input_columns: usize,
) -> AnyElement<'static> {
    if show_paste_prompt {
        let masked = masked_code(pasted_code);
        return element! {
            View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                #(forced_method_message.map(|message| element! {
                    View { Text(content: message, dim: true) }
                }))
                View(flex_direction: FlexDirection::Row) {
                    Text(content: PASTE_HERE_MSG.to_string(), wrap: TextWrap::NoWrap)
                    View(width: text_input_columns.max(1) as u32, overflow: Overflow::Hidden, height: 1u32) {
                        Text(content: masked, wrap: TextWrap::NoWrap)
                    }
                }
            }
        }
        .into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            #(forced_method_message.map(|message| element! {
                View { Text(content: message, dim: true) }
            }))
            View(flex_direction: FlexDirection::Row) {
                Spinner
                Text(content: "Opening browser to sign in…".to_string())
            }
        }
    }
    .into_any()
}

fn render_creating_api_key() -> AnyElement<'static> {
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            View(flex_direction: FlexDirection::Row) {
                Spinner
                Text(content: "Creating API key for Claude Code…".to_string())
            }
        }
    }
    .into_any()
}

fn render_success(
    mode: ConsoleOAuthMode,
    token: &Option<String>,
    email: &Option<String>,
    theme: Theme,
) -> AnyElement<'static> {
    if mode == ConsoleOAuthMode::SetupToken && token.is_some() {
        return element! { View(flex_direction: FlexDirection::Column) }.into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            #(email.as_ref().map(|email| element! {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "Logged in as ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    Text(content: email.clone(), wrap: TextWrap::NoWrap)
                }
            }))
            View(flex_direction: FlexDirection::Row) {
                Text(content: "Login successful. Press ".to_string(), color: theme.success, wrap: TextWrap::NoWrap)
                Text(content: "Enter".to_string(), color: theme.success, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                Text(content: " to continue…".to_string(), color: theme.success, wrap: TextWrap::NoWrap)
            }
        }
    }
    .into_any()
}

fn render_error(message: &str, can_retry: bool, theme: Theme) -> AnyElement<'static> {
    let message = message.to_string();
    element! {
        View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
            Text(content: format!("OAuth error: {message}"), color: theme.error)
            #(if can_retry {
                Some(element! {
                    View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                        Text(content: "Press ".to_string(), color: theme.permission, wrap: TextWrap::NoWrap)
                        Text(content: "Enter".to_string(), color: theme.permission, weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: " to retry.".to_string(), color: theme.permission, wrap: TextWrap::NoWrap)
                    }
                })
            } else {
                None
            })
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render_flow(props: ConsoleOAuthFlowProps) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ConsoleOAuthFlow(
                    oauth_status: props.oauth_status,
                    mode: props.mode,
                    starting_message: props.starting_message,
                    force_login_method: props.force_login_method,
                    show_paste_prompt: props.show_paste_prompt,
                    url_copied: props.url_copied,
                    pasted_code: props.pasted_code,
                    cursor_offset: props.cursor_offset,
                    text_input_columns: props.text_input_columns,
                )
            }
        }
        .render(Some(140))
        .to_string()
    }

    #[test]
    fn console_oauth_flow_idle_renders_official_login_options() {
        let text = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::Idle,
            ..ConsoleOAuthFlowProps::default()
        });

        assert!(
            text.contains("Claude Code can be used with your Claude subscription"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Select login method:"), "canvas=\n{text}");
        assert!(
            text.contains("Claude account with subscription"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Anthropic Console account"),
            "canvas=\n{text}"
        );
        assert!(text.contains("3rd-party platform"), "canvas=\n{text}");
    }

    #[test]
    fn console_oauth_flow_ready_creating_and_retry_states_match_official_copy() {
        let ready = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::ReadyToStart,
            ..ConsoleOAuthFlowProps::default()
        });
        assert!(
            ready.contains("Opening browser to sign in…"),
            "canvas=\n{ready}"
        );

        let creating = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::CreatingApiKey,
            ..ConsoleOAuthFlowProps::default()
        });
        assert!(
            creating.contains("Creating API key for Claude Code…"),
            "canvas=\n{creating}"
        );

        let retrying = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::AboutToRetry,
            ..ConsoleOAuthFlowProps::default()
        });
        assert!(retrying.contains("Retrying…"), "canvas=\n{retrying}");
    }

    #[test]
    fn console_oauth_flow_platform_setup_renders_docs_and_enter_hint() {
        let text = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::PlatformSetup,
            ..ConsoleOAuthFlowProps::default()
        });

        assert!(
            text.contains("Using 3rd-party platforms"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Amazon Bedrock"), "canvas=\n{text}");
        assert!(text.contains("Microsoft Foundry"), "canvas=\n{text}");
        assert!(text.contains("Vertex AI"), "canvas=\n{text}");
        assert!(
            text.contains("Press Enter to go back to login options."),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn console_oauth_flow_waiting_for_login_renders_copied_hint() {
        let text = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::WaitingForLogin {
                url: "https://example.test/oauth".to_string(),
            },
            show_paste_prompt: true,
            url_copied: true,
            force_login_method: Some(ForceLoginMethod::Console),
            ..ConsoleOAuthFlowProps::default()
        });

        assert!(text.contains("(Copied!)"), "canvas=\n{text}");
        assert!(
            text.contains("Login method pre-selected: API Usage Billing"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn console_oauth_flow_waiting_for_login_renders_copy_url_and_masked_prompt() {
        let text = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::WaitingForLogin {
                url: "https://example.test/oauth".to_string(),
            },
            show_paste_prompt: true,
            pasted_code: "abc#state".to_string(),
            text_input_columns: 40,
            force_login_method: Some(ForceLoginMethod::ClaudeAi),
            ..ConsoleOAuthFlowProps::default()
        });

        assert!(
            text.contains("Browser didn't open? Use the url below to sign in"),
            "canvas=\n{text}"
        );
        assert!(text.contains("(c to copy)"), "canvas=\n{text}");
        assert!(
            text.contains("https://example.test/oauth"),
            "canvas=\n{text}"
        );
        assert!(text.contains(PASTE_HERE_MSG), "canvas=\n{text}");
        assert!(text.contains("*********"), "canvas=\n{text}");
        assert!(
            text.contains("Login method pre-selected: Subscription Plan"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn console_oauth_flow_success_and_error_states_match_official_copy() {
        let success = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::Success {
                token: None,
                email: Some("user@example.com".to_string()),
            },
            ..ConsoleOAuthFlowProps::default()
        });
        assert!(
            success.contains("Logged in as user@example.com"),
            "canvas=\n{success}"
        );
        assert!(
            success.contains("Login successful. Press Enter to continue…"),
            "canvas=\n{success}"
        );

        let error = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::Error {
                message: "Invalid code".to_string(),
                can_retry: true,
            },
            ..ConsoleOAuthFlowProps::default()
        });
        assert!(
            error.contains("OAuth error: Invalid code"),
            "canvas=\n{error}"
        );
        assert!(error.contains("Press Enter to retry."), "canvas=\n{error}");
    }

    #[test]
    fn console_oauth_flow_setup_token_success_renders_token_without_continue_message() {
        let text = render_flow(ConsoleOAuthFlowProps {
            oauth_status: OAuthStatus::Success {
                token: Some("oauth-token".to_string()),
                email: None,
            },
            mode: ConsoleOAuthMode::SetupToken,
            ..ConsoleOAuthFlowProps::default()
        });

        assert!(
            text.contains("Long-lived authentication token created successfully"),
            "canvas=\n{text}"
        );
        assert!(text.contains("oauth-token"), "canvas=\n{text}");
        assert!(
            text.contains("CLAUDE_CODE_OAUTH_TOKEN=<token>"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Login successful. Press"), "canvas=\n{text}");
    }
}
