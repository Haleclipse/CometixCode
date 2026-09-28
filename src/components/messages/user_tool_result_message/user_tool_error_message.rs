//! Maps to: CC `components/messages/UserToolResultMessage/UserToolErrorMessage.tsx`.

use super::rejected_plan_message::RejectedPlanMessage;
use super::rejected_tool_use_message::RejectedToolUseMessage;
use crate::components::fallback_tool_use_error_message::FallbackToolUseErrorMessage;
use crate::components::interrupted_by_user::InterruptedByUser;
use crate::components::message_response::MessageResponse;
use crate::tools::{file_read_tool, grep_tool};
use crate::utils::messages::{
    INTERRUPT_MESSAGE_FOR_TOOL_USE, PLAN_REJECTION_PREFIX, REJECT_MESSAGE_WITH_REASON_PREFIX,
    is_classifier_denial,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct UserToolErrorMessageProps {
    pub tool_name: String,
    pub content: String,
    /// Maps to: CC `progressMessagesForMessage`
    /// (`UserToolErrorMessage.tsx:25`, forwarded into
    /// `tool.renderToolUseErrorMessage` at `:84-90` through
    /// `filterToolProgressMessages` — which drops only `hook_progress`
    /// payloads, a carrier the Rust progress enum does not have, so no filter
    /// runs here). Only the Agent arm consumes it today.
    pub progress_messages: Vec<crate::types::message::ToolUseProgressMessage>,
    pub verbose: bool,
    pub is_transcript_mode: bool,
}

/// Maps to: CC
/// `components/messages/UserToolResultMessage/UserToolErrorMessage.tsx:33-95`
/// `UserToolErrorMessage`.
#[component]
pub fn UserToolErrorMessage(
    props: &UserToolErrorMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let content = props.content.trim().to_string();

    if content.contains(INTERRUPT_MESSAGE_FOR_TOOL_USE) {
        return element! {
            MessageResponse(height: Some(1)) {
                InterruptedByUser
            }
        }
        .into_any();
    }
    if content.starts_with(PLAN_REJECTION_PREFIX) {
        let plan = content[PLAN_REJECTION_PREFIX.len()..].trim().to_string();
        return element! { RejectedPlanMessage(plan: plan) }.into_any();
    }
    if content.starts_with(REJECT_MESSAGE_WITH_REASON_PREFIX) {
        return element! { RejectedToolUseMessage }.into_any();
    }
    if is_classifier_denial(&content) {
        return element! {
            MessageResponse {
                Text(
                    content: "Denied by auto mode classifier · /feedback if incorrect".to_string(),
                    dim: true,
                    wrap: TextWrap::NoWrap,
                )
            }
        }
        .into_any();
    }

    // CC's per-tool error gates read ONLY `verbose` (FileEditTool UI.tsx:181,
    // FileWriteTool UI.tsx:269, NotebookEditTool UI.tsx:86); transcript mode
    // does not expand the compact copy.
    let error_verbose = props.verbose;
    // Maps to CC `UserToolErrorMessage.tsx:83-94` dispatching
    // `tool.renderToolUseErrorMessage` — AgentTool's implementation
    // (`tools/AgentTool/UI.tsx:765-789`) replays the progress transcript and
    // then the fallback. The body lives with the tool's UI owner.
    if props.tool_name.eq_ignore_ascii_case("Agent") || props.tool_name.eq_ignore_ascii_case("Task")
    {
        return crate::tools::agent_tool::ui::render_tool_use_error_message_element(
            &content,
            &props.progress_messages,
            props.verbose,
            props.is_transcript_mode,
        );
    }
    // Maps to CC FileReadTool `renderToolUseErrorMessage`: compact mode
    // owns only two stable summaries; every other error uses the official
    // fallback error renderer.
    if props.tool_name.eq_ignore_ascii_case("Read") {
        if let Some(error) =
            file_read_tool::ui::render_tool_use_error_message(&content, props.verbose)
        {
            return element! {
                MessageResponse {
                    Text(content: error.to_string(), color: theme.error, wrap: TextWrap::NoWrap)
                }
            }
            .into_any();
        }
        return element! {
            FallbackToolUseErrorMessage(
                result: Some(content),
                verbose: props.verbose,
            )
        }
        .into_any();
    }
    // Maps to CC FileEditTool `renderToolUseErrorMessage`: this must run
    // before the generic error fallback so collapsed validation errors use
    // the tool-specific stable copy.
    if matches!(props.tool_name.as_str(), "Edit" | "FileEdit") && !error_verbose {
        if let Some(error) = crate::utils::messages::extract_tag(&content, "tool_use_error") {
            let (label, color) = if error.contains("File has not been read yet") {
                ("File must be read first", theme.inactive)
            } else if error.contains(crate::utils::file::FILE_NOT_FOUND_CWD_NOTE) {
                ("File not found", theme.error)
            } else {
                ("Error editing file", theme.error)
            };
            return element! {
                MessageResponse {
                    Text(content: label.to_string(), color, wrap: TextWrap::NoWrap)
                }
            }
            .into_any();
        }
    }
    // Maps to CC FileWriteTool `renderToolUseErrorMessage` (UI.tsx:268-272):
    // collapsed tagged failures hide filesystem details behind a stable
    // summary. CC uses extractTag — an unclosed or empty tag falls through
    // to the Fallback, not the compact copy.
    if matches!(props.tool_name.as_str(), "Write" | "FileWrite")
        && !error_verbose
        && crate::utils::messages::extract_tag(&content, "tool_use_error")
            .is_some_and(|error| !error.is_empty())
    {
        return element! {
            MessageResponse {
                Text(
                    content: "Error writing file".to_string(),
                    color: theme.error,
                    wrap: TextWrap::NoWrap,
                )
            }
        }
        .into_any();
    }
    // Maps to CC NotebookEditTool `renderToolUseErrorMessage`: tagged
    // validation failures collapse in normal mode but remain detailed in
    // verbose/transcript mode.
    if props.tool_name.eq_ignore_ascii_case("NotebookEdit")
        && !error_verbose
        && crate::utils::messages::extract_tag(&content, "tool_use_error").is_some()
    {
        return element! {
            MessageResponse {
                Text(
                    content: "Error editing notebook".to_string(),
                    color: theme.error,
                    wrap: TextWrap::NoWrap,
                )
            }
        }
        .into_any();
    }
    // Maps to CC LSPTool `renderToolUseErrorMessage` (`LSPTool/UI.tsx:160-176`):
    // non-verbose tagged copy compacts to the stable summary, else Fallback.
    // CC reads only `verbose` here, not the transcript gate.
    if props.tool_name.eq_ignore_ascii_case("LSP") {
        if let Some(message) =
            crate::tools::lsp_tool::ui::render_tool_use_error_message(&content, props.verbose)
        {
            return element! {
                MessageResponse {
                    Text(content: message, color: theme.error, wrap: TextWrap::NoWrap)
                }
            }
            .into_any();
        }
        return element! {
            FallbackToolUseErrorMessage(
                result: Some(content),
                verbose: props.verbose,
            )
        }
        .into_any();
    }
    // Maps to CC SyntheticOutputTool `renderToolUseErrorMessage`
    // (`SyntheticOutputTool.ts:85-87`): the bare constant string — React
    // renders the naked return with no MessageResponse wrapper or color.
    if props.tool_name.eq_ignore_ascii_case("StructuredOutput") {
        return element! {
            Text(
                content: crate::tools::synthetic_output_tool::ui::render_error_message()
                    .to_string(),
                wrap: TextWrap::NoWrap,
            )
        }
        .into_any();
    }
    // Maps to CC Grep/Glob `renderToolUseErrorMessage` before Fallback.
    let is_search_tool = matches!(props.tool_name.as_str(), "Grep" | "Glob" | "Search");
    if is_search_tool {
        if let Some(short) = grep_tool::ui::search_tool_use_error_message(&content, props.verbose) {
            return element! {
                MessageResponse {
                    Text(content: short.to_string(), color: theme.error, wrap: TextWrap::NoWrap)
                }
            }
            .into_any();
        }
    }
    let fallback_verbose = if is_search_tool {
        props.verbose
    } else {
        error_verbose
    };
    element! {
        FallbackToolUseErrorMessage(
            result: Some(content),
            verbose: fallback_verbose,
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(tool_name: &str, content: &str, verbose: bool) -> String {
        use iocraft::prelude::ElementExt as _;
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolErrorMessage(
                    tool_name: tool_name.to_string(),
                    content: content.to_string(),
                    verbose: verbose,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(80))
        .to_string()
    }

    /// Maps to: CC `LSPTool/UI.tsx:160-176` — a non-verbose tagged failure
    /// compacts to "LSP operation failed"; verbose falls back to the full
    /// error copy. The component leaf must agree with the line pipeline.
    #[test]
    fn lsp_error_leaf_compacts_tagged_copy_like_official() {
        let tagged =
            "<tool_use_error>LSP request failed: server crashed with a long trace</tool_use_error>";
        let compact = render("LSP", tagged, false);
        assert!(
            compact.contains("LSP operation failed"),
            "canvas=\n{compact}"
        );
        assert!(!compact.contains("server crashed"), "canvas=\n{compact}");

        let verbose = render("LSP", tagged, true);
        assert!(verbose.contains("server crashed"), "canvas=\n{verbose}");
    }

    /// Maps to: CC `SyntheticOutputTool.ts:85-87` — the error renderer is the
    /// bare constant string regardless of verbosity.
    #[test]
    fn structured_output_error_leaf_is_the_bare_constant() {
        for verbose in [false, true] {
            let canvas = render(
                "StructuredOutput",
                "<tool_use_error>Output does not match required schema</tool_use_error>",
                verbose,
            );
            assert!(
                canvas.contains("Structured output error"),
                "canvas=\n{canvas}"
            );
            assert!(!canvas.contains("required schema"), "canvas=\n{canvas}");
        }
    }
}
