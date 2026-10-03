//! Maps to: CC `components/messages/UserTextMessage.tsx`.
//! Important: official Claude Code keeps user text blocks as raw text and lets
//! this component dispatch only on explicit XML markers. Do not reinterpret
//! arbitrary prompts that merely start with `/` or `$`; those are still normal
//! user prompts unless they were recorded with command/bash tags.

use super::user_agent_notification_message::UserAgentNotificationMessage;
use super::user_bash_input_message::UserBashInputMessage;
use super::user_bash_output_message::UserBashOutputMessage;
use super::user_channel_message::UserChannelMessage;
use super::user_command_message::UserCommandMessage;
use super::user_local_command_output_message::UserLocalCommandOutputMessage;
use super::user_memory_input_message::UserMemoryInputMessage;
use super::user_plan_message::UserPlanMessage;
use super::user_prompt_message::UserPromptMessage;
use super::user_resource_update_message::UserResourceUpdateMessage;
use super::user_teammate_message::UserTeammateMessage;
use crate::components::message_response::MessageResponse;
// Maps to: CC UserTextMessage.tsx — tags come from constants/xml. (CC spells
// bash-stdout / bash-stderr / local-command-stdout / local-command-stderr /
// bash-input as literals there; all live in constants/xml.ts, so they are
// imported alongside.)
use crate::constants::xml::{
    BASH_INPUT_TAG, BASH_STDERR_TAG, BASH_STDOUT_TAG, COMMAND_ARGS_TAG, COMMAND_MESSAGE_TAG,
    FORK_BOILERPLATE_TAG, FORK_DIRECTIVE_PREFIX, LOCAL_COMMAND_CAVEAT_TAG,
    LOCAL_COMMAND_STDERR_TAG, LOCAL_COMMAND_STDOUT_TAG, TASK_NOTIFICATION_TAG,
    TEAMMATE_MESSAGE_TAG, TICK_TAG,
};
use crate::utils::messages::{INTERRUPT_MESSAGE, INTERRUPT_MESSAGE_FOR_TOOL_USE, extract_tag};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

const NO_CONTENT_MESSAGE: &str = "(no content)";

#[derive(Default, Props)]
pub struct UserTextMessageProps {
    pub content: String,
    /// CC `UserTextMessage.tsx:35` `planContent?: string` — the envelope's
    /// `UserMessage.planContent`, rendered before any tag dispatch.
    pub plan_content: Option<String>,
    pub add_margin: bool,
    pub verbose: bool,
    pub is_transcript_mode: bool,
    pub use_brief_layout: bool,
}

#[component]
pub fn UserTextMessage(
    props: &UserTextMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let text = props.content.as_str();

    if text.trim() == NO_CONTENT_MESSAGE {
        return element! { View }.into_any();
    }

    // CC UserTextMessage.tsx:53-54: planContent renders before the tag
    // dispatch (but after the NO_CONTENT early-out above).
    if let Some(plan_content) = props.plan_content.as_ref().filter(|plan| !plan.is_empty()) {
        return element! {
            UserPlanMessage(content: plan_content.clone(), add_margin: props.add_margin)
        }
        .into_any();
    }

    if extract_tag(text, TICK_TAG).is_some()
        || text.contains(&format!("<{LOCAL_COMMAND_CAVEAT_TAG}>"))
    {
        return element! { View }.into_any();
    }

    if starts_with_tag(text, BASH_STDOUT_TAG) || starts_with_tag(text, BASH_STDERR_TAG) {
        let stdout = extract_tag(text, BASH_STDOUT_TAG).unwrap_or_default();
        let stderr = extract_tag(text, BASH_STDERR_TAG).unwrap_or_default();
        let (output, is_error) = if !stderr.trim().is_empty() {
            (stderr, true)
        } else {
            (stdout, false)
        };
        return element! {
            UserBashOutputMessage(output: output, is_error: is_error, add_margin: props.add_margin)
        }
        .into_any();
    }

    if starts_with_tag(text, LOCAL_COMMAND_STDOUT_TAG)
        || starts_with_tag(text, LOCAL_COMMAND_STDERR_TAG)
    {
        let stdout = extract_tag(text, LOCAL_COMMAND_STDOUT_TAG).unwrap_or_default();
        let stderr = extract_tag(text, LOCAL_COMMAND_STDERR_TAG).unwrap_or_default();
        let mut parts = Vec::new();
        if !stdout.trim().is_empty() {
            parts.push(stdout.trim().to_string());
        }
        if !stderr.trim().is_empty() {
            parts.push(stderr.trim().to_string());
        }
        return element! {
            UserLocalCommandOutputMessage(
                output: parts.join("\n"),
                is_error: !stderr.trim().is_empty(),
                add_margin: props.add_margin,
            )
        }
        .into_any();
    }

    if text == INTERRUPT_MESSAGE || text == INTERRUPT_MESSAGE_FOR_TOOL_USE {
        return element! {
            MessageResponse(content: "Interrupted by user".to_string(), color: Some(theme.inactive))
        }
        .into_any();
    }

    if let Some(input) = extract_tag(text, BASH_INPUT_TAG) {
        return element! {
            UserBashInputMessage(command: input, add_margin: props.add_margin)
        }
        .into_any();
    }

    if let Some(command) = extract_tag(text, COMMAND_MESSAGE_TAG) {
        let args = extract_tag(text, COMMAND_ARGS_TAG).unwrap_or_default();
        return element! {
            UserCommandMessage(command: command, args: args, add_margin: props.add_margin)
        }
        .into_any();
    }

    if text.contains("<user-memory-input>") {
        return element! {
            UserMemoryInputMessage(text: text.to_string(), add_margin: props.add_margin)
        }
        .into_any();
    }

    // No closing `>`: the tag may carry attributes, matching CC's
    // `includes(`<${TASK_NOTIFICATION_TAG}`)`.
    if text.contains(&format!("<{TASK_NOTIFICATION_TAG}")) {
        // CC hands the whole param to `UserAgentNotificationMessage`, which
        // extracts summary/status itself and returns null without a summary —
        // no invented fallback copy here.
        return element! {
            UserAgentNotificationMessage(text: text.to_string(), add_margin: props.add_margin)
        }
        .into_any();
    }

    if text.contains("<mcp-resource-update") || text.contains("<mcp-polling-update") {
        return element! {
            UserResourceUpdateMessage(content: text.to_string(), add_margin: props.add_margin)
        }
        .into_any();
    }

    // Maps to: CC `UserTextMessage.tsx:151-162`. The gate is the RAW build
    // feature (`feature('FORK_SUBAGENT')`), not `isForkSubagentEnabled()` —
    // CC's own comment there says the tag literal is inlined "so the import
    // doesn't ship in external builds where feature('FORK_SUBAGENT') is
    // false", i.e. it is a build-time dead-code gate. A transcript can be
    // replayed in a session whose runtime vetoes (coordinator /
    // non-interactive) differ from the one that produced it, so the renderer
    // must not consult them.
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::ForkSubagent,
    ) && text.contains(&format!("<{FORK_BOILERPLATE_TAG}>"))
    {
        return element! {
            UserPromptMessage(
                content: collapse_fork_boilerplate(text),
                add_margin: props.add_margin,
                use_brief_layout: props.use_brief_layout,
                is_transcript_mode: props.is_transcript_mode,
            )
        }
        .into_any();
    }

    if text.contains(&format!("<{TEAMMATE_MESSAGE_TAG}")) {
        return element! {
            UserTeammateMessage(
                sender: "teammate".to_string(),
                content: text.to_string(),
                add_margin: props.add_margin,
                is_transcript_mode: props.is_transcript_mode,
            )
        }
        .into_any();
    }

    if text.contains("<channel ") {
        let source = extract_attr(text, "source").unwrap_or_else(|| "channel".to_string());
        let content = extract_tag(text, "channel").unwrap_or_default();
        return element! {
            UserChannelMessage(source: source, content: content.trim().to_string(), add_margin: props.add_margin)
        }
        .into_any();
    }

    element! {
        UserPromptMessage(
            content: props.content.clone(),
            add_margin: props.add_margin,
            use_brief_layout: props.use_brief_layout,
            is_transcript_mode: props.is_transcript_mode,
        )
    }
    .into_any()
}

/// Stands in for CC `components/messages/UserForkBoilerplateMessage.tsx`.
///
/// APPROXIMATION, deliberate. That component has NO source in `../rebuild` —
/// `UserTextMessage.tsx:157-158` `require()`s it, and the file is absent, so
/// there is no body to port and none is invented here. What IS in CC source is
/// its observable contract, stated twice:
///
/// - `constants/xml.ts:60-62`: the tag "wraps the rules/format boilerplate in a
///   fork child's first message. Lets the transcript renderer collapse the
///   boilerplate and show only the directive."
/// - `constants/xml.ts:64-66`: `FORK_DIRECTIVE_PREFIX` is the "prefix before
///   the directive text, stripped by the renderer. Keep in sync across
///   buildChildMessage (generates) and UserForkBoilerplateMessage (parses)."
/// - `UserTextMessage.tsx:151-152`: "collapse the rules/format boilerplate,
///   show only the directive."
///
/// So the two text transforms are CC-specified: drop the `<fork-boilerplate>`
/// element, strip `FORK_DIRECTIVE_PREFIX`. Only the PRESENTATION of the
/// remainder is unknown, and this port invents nothing there either — the
/// residual directive goes to [`UserPromptMessage`], the very component the
/// same text reaches when the branch is not taken. A future scout with the real
/// component may find extra chrome (a collapsed-lines hint, a distinct label);
/// that is the open half of this seam. What is NOT open: rendering the 30-line
/// rules block verbatim, which is the one thing CC's source rules out.
///
/// Input shape is `forkSubagent.ts#buildChildMessage` exactly (the fork child's
/// first user text block). A message carrying the opening tag without a closing
/// one is not a shape CC produces; it is returned untouched rather than
/// swallowed.
fn collapse_fork_boilerplate(text: &str) -> String {
    let close = format!("</{FORK_BOILERPLATE_TAG}>");
    let Some(close_at) = text.find(&close) else {
        return text.to_string();
    };
    let directive = text[close_at + close.len()..].trim_start();
    directive
        .strip_prefix(FORK_DIRECTIVE_PREFIX)
        .unwrap_or(directive)
        .to_string()
}

fn starts_with_tag(text: &str, tag: &str) -> bool {
    text.trim_start().starts_with(&format!("<{tag}"))
}

fn extract_attr(text: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = text.find(&needle)? + needle.len();
    let end = text[start..].find('"')? + start;
    Some(text[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn render_user_text(content: &str, is_transcript_mode: bool) -> String {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserTextMessage(
                    content: content.to_string(),
                    is_transcript_mode: is_transcript_mode,
                )
            }
        }
        .render(None)
        .to_string()
    }

    #[test]
    fn user_text_forwards_raw_teammate_xml_and_transcript_mode() {
        let content = r#"<teammate-message teammate_id="alice" summary="Brief update">body one</teammate-message>"#;

        let compact = render_user_text(content, false);
        assert!(compact.contains("@alice"));
        assert!(compact.contains("Brief update"));
        assert!(!compact.contains("body one"));

        let transcript = render_user_text(content, true);
        assert!(transcript.contains("@alice"));
        assert!(transcript.contains("body one"));
    }

    /// Maps to: CC `UserTextMessage.tsx:151-162`. Before this branch existed the
    /// fork child's first message fell through to `UserPromptMessage` and
    /// rendered the whole `<fork-boilerplate>` element verbatim — 30 rows at
    /// width 100, of which one carried the directive.
    #[test]
    fn fork_boilerplate_collapses_to_the_directive() {
        let content =
            crate::tools::agent_tool::fork_subagent::build_child_message("audit the auth flow");
        let rendered = render_user_text(&content, true);

        assert!(rendered.contains("audit the auth flow"));
        assert!(!rendered.contains("fork-boilerplate"));
        assert!(!rendered.contains("STOP. READ THIS FIRST."));
        assert!(!rendered.contains("RULES (non-negotiable)"));
        assert!(!rendered.contains("Output format"));
        // `constants/xml.ts:64-66`: the prefix is "stripped by the renderer".
        assert!(!rendered.contains("Your directive:"));
        assert_eq!(
            rendered
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count(),
            1,
            "collapsed message is one row, got:\n{rendered}"
        );
    }

    /// CC gates this branch on the RAW `feature('FORK_SUBAGENT')`
    /// (`UserTextMessage.tsx:154`), which its bundler resolves at build time —
    /// not on `isForkSubagentEnabled()`. A transcript can be replayed in a
    /// session whose coordinator / non-interactive state differs from the one
    /// that produced it, so holding the runtime veto must not bring the raw
    /// boilerplate back.
    #[test]
    fn fork_boilerplate_collapse_ignores_the_runtime_vetoes() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _headless = crate::tools::agent_tool::fork_subagent::fork_veto_environment();
        assert!(
            !crate::tools::agent_tool::fork_subagent::is_fork_subagent_enabled(),
            "the runtime gate must be OFF for this test to say anything"
        );

        let content =
            crate::tools::agent_tool::fork_subagent::build_child_message("re-read the config");
        let rendered = render_user_text(&content, true);
        assert!(rendered.contains("re-read the config"));
        assert!(!rendered.contains("STOP. READ THIS FIRST."));
    }

    /// `buildChildMessage` never emits an unterminated element, so the collapse
    /// returns such text untouched rather than swallowing it.
    #[test]
    fn fork_boilerplate_without_a_closing_tag_is_left_intact() {
        let text = "<fork-boilerplate>\nSTOP. READ THIS FIRST.\n";
        assert_eq!(collapse_fork_boilerplate(text), text);
        assert!(render_user_text(text, true).contains("STOP. READ THIS FIRST."));
    }

    /// A directive that is itself empty leaves nothing to show; the boilerplate
    /// must still not leak through.
    #[test]
    fn fork_boilerplate_collapse_strips_the_prefix_even_with_an_empty_directive() {
        let content = crate::tools::agent_tool::fork_subagent::build_child_message("");
        assert_eq!(collapse_fork_boilerplate(&content), "");
    }
}
