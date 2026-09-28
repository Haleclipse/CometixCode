//! Maps to: CC `components/messages/AssistantTextMessage.tsx`.

use crate::components::ctrl_o_to_expand::ctrl_o_to_expand_hint;
use crate::components::interrupted_by_user::InterruptedByUser;
use crate::components::markdown::{Markdown, StreamingMarkdown};
use crate::components::message_response::MessageResponse;
use crate::components::messages::rate_limit_message::RateLimitMessage;
use crate::utils::messages::{NO_RESPONSE_REQUESTED, is_empty_message_text};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

const BLACK_CIRCLE: &str = if cfg!(target_os = "macos") {
    "⏺"
} else {
    "●"
};
const MAX_API_ERROR_CHARS: usize = 1000;
const API_ERROR_MESSAGE_PREFIX: &str = "API Error";
const PROMPT_TOO_LONG_ERROR_MESSAGE: &str = "Prompt is too long";
const CREDIT_BALANCE_TOO_LOW_ERROR_MESSAGE: &str = "Credit balance is too low";
const INVALID_API_KEY_ERROR_MESSAGE: &str = "Not logged in · Please run /login";
const INVALID_API_KEY_ERROR_MESSAGE_EXTERNAL: &str = "Invalid API key · Fix external API key";
const ORG_DISABLED_ERROR_MESSAGE_ENV_KEY_WITH_OAUTH: &str = "Your ANTHROPIC_API_KEY belongs to a disabled organization · Unset the environment variable to use your subscription instead";
const ORG_DISABLED_ERROR_MESSAGE_ENV_KEY: &str = "Your ANTHROPIC_API_KEY belongs to a disabled organization · Update or unset the environment variable";
const TOKEN_REVOKED_ERROR_MESSAGE: &str = "OAuth token revoked · Please run /login";
const CUSTOM_OFF_SWITCH_MESSAGE: &str =
    "Opus is experiencing high load, please use /model to switch to Sonnet";
const API_TIMEOUT_ERROR_MESSAGE: &str = "Request timed out";
const ERROR_MESSAGE_USER_ABORT: &str = "API Error: Request was aborted.";

#[derive(Default, Props)]
pub struct AssistantTextMessageProps {
    pub content: String,
    pub add_margin: bool,
    pub should_show_dot: bool,
    pub verbose: bool,
    /// Maps to: CC `AssistantMessage.isApiErrorMessage` (`types/message.ts:41`).
    /// Authoritative when set; the prefix sniff below is the fallback for rows
    /// restored from transcripts written before the flag existed.
    pub is_api_error_message: bool,
}

#[component]
pub fn AssistantTextMessage(
    props: &AssistantTextMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let text = props.content.as_str();

    if is_empty_message_text(text) || text == NO_RESPONSE_REQUESTED {
        return element! { View }.into_any();
    }

    if is_rate_limit_error_message(text) {
        return element! {
            RateLimitMessage(text: text.to_string(), hint: String::new(), add_margin: props.add_margin)
        }
        .into_any();
    }

    if text == ERROR_MESSAGE_USER_ABORT {
        return element! {
            MessageResponse(height: Some(1)) {
                InterruptedByUser
            }
        }
        .into_any();
    }

    if let Some(content) =
        assistant_text_special_response(text, props.verbose, props.is_api_error_message)
    {
        return element! {
            MessageResponse(content: content, color: Some(theme.error))
        }
        .into_any();
    }

    element! {
        View(
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::FLEX_START,
            margin_top: if props.add_margin { 1u32 } else { 0u32 },
            width: 100pct,
        ) {
            #(if props.should_show_dot {
                Some(element! {
                    View(min_width: 2u32, flex_shrink: 0.0f32) {
                        Text(content: BLACK_CIRCLE, color: theme.text, wrap: TextWrap::NoWrap)
                    }
                })
            } else {
                None
            })
            View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                Markdown(content: props.content.clone())
            }
        }
    }
    .into_any()
}

#[derive(Default, Props)]
pub struct StreamingAssistantTextMessageProps {
    pub content: String,
    pub add_margin: bool,
}

/// Maps to: CC `components/Messages.tsx` streamingText sibling
/// (`StreamingMarkdown` under the assistant dot). This is a transient preview,
/// not a formal transcript row.
#[component]
pub fn StreamingAssistantTextMessage(
    props: &StreamingAssistantTextMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    if props.content.is_empty() {
        return element! { View }.into_any();
    }

    element! {
        View(
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::FLEX_START,
            margin_top: if props.add_margin { 1u32 } else { 0u32 },
            width: 100pct,
        ) {
            View(min_width: 2u32, flex_shrink: 0.0f32) {
                Text(content: BLACK_CIRCLE, color: theme.text, wrap: TextWrap::NoWrap)
            }
            View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                StreamingMarkdown(content: props.content.clone())
            }
        }
    }
    .into_any()
}

fn starts_with_api_error_prefix(text: &str) -> bool {
    text.starts_with(API_ERROR_MESSAGE_PREFIX) || text.starts_with("Please run /login · API Error")
}

fn is_rate_limit_error_message(text: &str) -> bool {
    [
        "You've hit your",
        "You've used",
        "You're now using extra usage",
        "You're close to",
        "You're out of extra usage",
    ]
    .into_iter()
    .any(|prefix| text.starts_with(prefix))
}

fn truncate_api_error(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= MAX_API_ERROR_CHARS {
        return text.to_string();
    }
    format!(
        "{}…\n{}",
        text.chars().take(MAX_API_ERROR_CHARS).collect::<String>(),
        ctrl_o_to_expand_hint()
    )
}

fn assistant_text_special_response(
    text: &str,
    verbose: bool,
    is_api_error_message: bool,
) -> Option<String> {
    match text {
        PROMPT_TOO_LONG_ERROR_MESSAGE => {
            Some("Context limit reached · /compact or /clear to continue".to_string())
        }
        CREDIT_BALANCE_TOO_LOW_ERROR_MESSAGE => Some(
            "Credit balance too low · Add funds: https://platform.claude.com/settings/billing"
                .to_string(),
        ),
        INVALID_API_KEY_ERROR_MESSAGE
        | INVALID_API_KEY_ERROR_MESSAGE_EXTERNAL
        | ORG_DISABLED_ERROR_MESSAGE_ENV_KEY
        | ORG_DISABLED_ERROR_MESSAGE_ENV_KEY_WITH_OAUTH
        | TOKEN_REVOKED_ERROR_MESSAGE => Some(text.to_string()),
        API_TIMEOUT_ERROR_MESSAGE => Some(match std::env::var("API_TIMEOUT_MS") {
            Ok(ms) if !ms.trim().is_empty() => {
                format!("{API_TIMEOUT_ERROR_MESSAGE} (API_TIMEOUT_MS={ms}ms, try increasing it)")
            }
            _ => API_TIMEOUT_ERROR_MESSAGE.to_string(),
        }),
        CUSTOM_OFF_SWITCH_MESSAGE => Some(
            "We are experiencing high demand for Opus 4.\nTo continue immediately, use /model to switch to Sonnet and continue coding."
                .to_string(),
        ),
        _ if is_api_error_message || starts_with_api_error_prefix(text) => Some(if text == API_ERROR_MESSAGE_PREFIX {
            "API Error: Please wait a moment and try again.".to_string()
        } else if verbose {
            text.to_string()
        } else {
            truncate_api_error(text)
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_assistant_text(content: &str, verbose: bool) -> String {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantTextMessage(
                    content: content.to_string(),
                    should_show_dot: true,
                    verbose: verbose,
                )
            }
        }
        .render(None)
        .to_string()
    }

    #[test]
    fn assistant_text_hides_empty_and_no_response_requested_like_official() {
        assert!(
            render_assistant_text(NO_RESPONSE_REQUESTED, false)
                .trim()
                .is_empty()
        );
        assert!(
            render_assistant_text("<context>hidden</context>", false)
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn assistant_text_renders_prompt_too_long_as_message_response() {
        let rendered = render_assistant_text(PROMPT_TOO_LONG_ERROR_MESSAGE, false);
        assert!(rendered.contains("⎿"));
        assert!(rendered.contains("Context limit reached · /compact or /clear to continue"));
        assert!(!rendered.contains(BLACK_CIRCLE));
    }

    #[test]
    fn assistant_text_routes_rate_limit_messages_to_rate_limit_renderer() {
        let rendered =
            render_assistant_text("You've hit your weekly limit · resets tomorrow", false);
        assert!(rendered.contains("⎿"));
        assert!(rendered.contains("You've hit your weekly limit"));
        assert!(!rendered.contains(BLACK_CIRCLE));
    }

    #[test]
    fn assistant_text_renders_and_truncates_api_errors_like_official() {
        let rendered = render_assistant_text(API_ERROR_MESSAGE_PREFIX, false);
        assert!(rendered.contains("API Error: Please wait a moment and try again."));

        let long = format!("API Error: {}", "x".repeat(MAX_API_ERROR_CHARS + 50));
        let truncated = render_assistant_text(&long, false);
        assert!(truncated.contains("…"));
        assert!(truncated.contains("ctrl+o to expand"));

        let verbose = render_assistant_text(&long, true);
        assert!(!verbose.contains("ctrl+o to expand"));
    }

    #[test]
    fn assistant_text_renders_user_abort_as_interrupted_message() {
        let rendered = render_assistant_text(ERROR_MESSAGE_USER_ABORT, false);
        assert!(rendered.contains("Interrupted · What should Claude do instead?"));
    }
}
