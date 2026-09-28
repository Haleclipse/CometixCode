//! Maps to: CC `components/messages/UserToolResultMessage/UserToolRejectMessage.tsx`.

use super::official_file_result_element;
use super::utils::{ToolRenderLine, ToolRenderTone};
use crate::components::fallback_tool_use_rejected_message::FallbackToolUseRejectedMessage;
use crate::components::notebook_edit_tool_use_rejected_message::NotebookEditToolUseRejectedMessage;
use crate::types::message::ToolResultStatus;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct UserToolRejectMessageProps {
    pub tool_name: String,
    /// CC `UserToolRejectMessage.tsx:14` `input` — resolved from the paired
    /// tool_use in the lookups, exactly like the tool name.
    pub tool_input: Option<serde_json::Value>,
    /// Maps to: CC `progressMessagesForMessage`
    /// (`UserToolRejectMessage.tsx:15`, forwarded into
    /// `tool.renderToolUseRejectedMessage` at `:51-53` through
    /// `filterToolProgressMessages` — which drops only `hook_progress`
    /// payloads, a carrier the Rust progress enum does not have, so no filter
    /// runs here). Only the Agent arm consumes it today.
    pub progress_messages: Vec<crate::types::message::ToolUseProgressMessage>,
    pub verbose: bool,
    /// Maps to: CC `isTranscriptMode` (`UserToolRejectMessage.tsx:21`).
    pub is_transcript_mode: bool,
    /// Maps to: CC `style?: 'condensed'` — forwarded to the per-tool
    /// `renderToolUseRejectedMessage`.
    pub style: Option<String>,
}

/// The line-producing body of CC `UserToolRejectMessage.tsx:46-58` for the
/// migrated tools whose rejected renderer emits plain copy —
/// `tool.renderToolUseRejectedMessage(parsedInput.data, …)`; `None` falls to
/// `FallbackToolUseRejectedMessage`.
pub(crate) fn render_tool_use_rejected_lines(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> Option<Vec<ToolRenderLine>> {
    if tool_name.eq_ignore_ascii_case("AskUserQuestion") {
        // CC `AskUserQuestionTool.tsx:285-292` — ignores the input.
        return Some(crate::tools::ask_user_question_tool::ui::render_rejected_result_lines());
    }
    if tool_name.eq_ignore_ascii_case("EnterPlanMode") {
        // CC `EnterPlanModeTool/UI.tsx:34-41` — ignores the input.
        return Some(crate::tools::enter_plan_mode_tool::ui::render_rejected_result_lines());
    }
    if tool_name.eq_ignore_ascii_case("Config") {
        // CC `ConfigTool/UI.tsx:46-48` — the constant warning-colored string,
        // input ignored.
        return Some(vec![ToolRenderLine::new(
            crate::tools::config_tool::ui::render_rejected_message(),
            ToolRenderTone::Warning,
        )]);
    }
    if tool_name.eq_ignore_ascii_case("StructuredOutput") {
        // CC `SyntheticOutputTool.ts:82-84` — the constant string, input
        // ignored.
        return Some(vec![ToolRenderLine::new(
            crate::tools::synthetic_output_tool::ui::render_rejected_message(),
            ToolRenderTone::Normal,
        )]);
    }
    if tool_name.eq_ignore_ascii_case("ExitPlanMode") {
        // CC `ExitPlanModeTool/UI.tsx:77-88` — `plan ?? getPlan() ?? 'No
        // plan found'`: the input plan first, then the session plan file,
        // then the constant.
        let plan = tool_input
            .and_then(|input| input.get("plan"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| crate::utils::plans::get_plan(None))
            .unwrap_or_else(|| "No plan found".to_string());
        return Some(crate::tools::exit_plan_mode_tool::ui::render_rejected_result_lines(&plan));
    }
    None
}

/// The name→inputSchema registry for the rejected renderers — same rationale
/// as `migrated_output_parses` (mod.rs): CC hangs `inputSchema` on each Tool
/// object and `UserToolRejectMessage.tsx:40-43` safeParses the paired input
/// BEFORE dispatching to `renderToolUseRejectedMessage`; a failed parse falls
/// back. Only the tools this dispatcher renders need entries; an unknown name
/// is CC's `!tool` branch and already falls back below.
fn migrated_input_schema(tool_name: &str) -> Option<&'static crate::utils::zod::Schema> {
    match tool_name.to_ascii_lowercase().as_str() {
        "edit" | "multiedit" | "fileedit" => Some(crate::tools::file_edit_tool::input_schema()),
        "write" | "filewrite" => Some(crate::tools::file_write_tool::input_schema()),
        "notebookedit" => Some(crate::tools::notebook_edit_tool::input_schema()),
        "askuserquestion" => Some(crate::tools::ask_user_question_tool::input_schema()),
        "enterplanmode" => Some(crate::tools::enter_plan_mode_tool::input_schema()),
        "config" => Some(crate::tools::config_tool::input_schema()),
        "structuredoutput" => Some(crate::tools::synthetic_output_tool::input_schema()),
        "exitplanmode" => Some(crate::tools::exit_plan_mode_tool::input_schema()),
        // AgentTool (legacy alias Task) — its rejected renderer replays
        // progress (`tools/AgentTool/UI.tsx:723-763`), gated by the same
        // safeParse as every other tool (`UserToolRejectMessage.tsx:40-43`).
        "agent" | "task" => Some(crate::tools::agent_tool::input_schema()),
        _ => None,
    }
}

/// Maps to: CC
/// `components/messages/UserToolResultMessage/UserToolRejectMessage.tsx:24-59`
/// `UserToolRejectMessage`.
#[component]
pub fn UserToolRejectMessage(
    props: &UserToolRejectMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();

    // CC `UserToolRejectMessage.tsx:40-43`: `tool.inputSchema.safeParse(input)`
    // gates every per-tool rejected renderer. The schemas carry no `.default`
    // fills (strict objects with plain `.optional()`s), so a successful parse
    // returns the input unchanged and the dispatch below keeps the original.
    if let Some(schema) = migrated_input_schema(&props.tool_name) {
        let input = props.tool_input.clone().unwrap_or(serde_json::Value::Null);
        if crate::utils::zod::safe_parse(schema, &input).is_err() {
            return element! { FallbackToolUseRejectedMessage }.into_any();
        }
    }

    // Maps to CC `UserToolRejectMessage.tsx:45-58` dispatching
    // `tool.renderToolUseRejectedMessage` — AgentTool's implementation
    // (`tools/AgentTool/UI.tsx:723-763`) replays the progress transcript and
    // then `FallbackToolUseRejectedMessage`; it ignores the parsed input on
    // external builds. The body lives with the tool's UI owner.
    if props.tool_name.eq_ignore_ascii_case("Agent") || props.tool_name.eq_ignore_ascii_case("Task")
    {
        return crate::tools::agent_tool::ui::render_tool_use_rejected_message_element(
            &props.progress_messages,
            props.verbose,
            props.is_transcript_mode,
        );
    }

    // The file tools compute their rejected diff preview from the paired
    // tool_use input at render time (CC renderToolUseRejectedMessage,
    // FileEditTool/UI.tsx:110-171 / FileWriteTool/UI.tsx:138-150).
    if let Some(element) = official_file_result_element(
        &props.tool_name,
        ToolResultStatus::Rejected,
        None,
        props.tool_input.as_ref(),
        props.verbose,
        props.style.as_deref(),
    ) {
        return element;
    }

    // CC `NotebookEditTool/UI.tsx:56-79` renders its rejected leaf entirely
    // from the input via a dedicated component.
    if props.tool_name.eq_ignore_ascii_case("NotebookEdit") {
        if let Some(input) = props.tool_input.as_ref() {
            let string = |key: &str| {
                input
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            };
            return element! {
                NotebookEditToolUseRejectedMessage(
                    notebook_path: string("notebook_path").unwrap_or_default(),
                    cell_id: string("cell_id"),
                    new_source: string("new_source").unwrap_or_default(),
                    cell_type: string("cell_type"),
                    edit_mode: string("edit_mode"),
                    verbose: props.verbose,
                )
            }
            .into_any();
        }
    }

    // CC `UserToolRejectMessage.tsx:46-58`: the tool's own rejected renderer
    // wins; everything else falls to `FallbackToolUseRejectedMessage`.
    if let Some(lines) = render_tool_use_rejected_lines(&props.tool_name, props.tool_input.as_ref())
    {
        return element! {
            View(flex_direction: FlexDirection::Column) {
                #(lines.into_iter().map(|line| {
                    let line_color = match line.tone {
                        ToolRenderTone::Normal => None,
                        ToolRenderTone::Success => Some(theme.success),
                        ToolRenderTone::Warning => Some(theme.warning),
                        ToolRenderTone::Error => Some(theme.error),
                        ToolRenderTone::Inactive => Some(theme.inactive),
                    };
                    element! {
                        Text(
                            content: line.text.clone(),
                            color: line_color,
                            wrap: TextWrap::Wrap,
                        )
                    }
                }).collect::<Vec<_>>())
            }
        }
        .into_any();
    }

    element! { FallbackToolUseRejectedMessage }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iocraft::prelude::ElementExt as _;

    fn render_reject(tool_name: &str, tool_input: Option<serde_json::Value>) -> String {
        let current_theme = *crate::utils::theme::current();
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                UserToolRejectMessage(
                    tool_name: tool_name.to_string(),
                    tool_input: tool_input,
                )
            }
        }
        .render(Some(80))
        .to_string()
    }

    /// CC `UserToolRejectMessage.tsx:40-43`: `tool.inputSchema.safeParse(input)`
    /// gates the per-tool rejected renderer; a failed parse falls back to
    /// `FallbackToolUseRejectedMessage` (`INTERRUPTED_BY_USER_TEXT`).
    #[test]
    fn schema_failures_fall_back_before_the_per_tool_renderer_like_official() {
        // Required key missing (NotebookEdit.notebook_path / new_source).
        let missing_required = render_reject(
            "NotebookEdit",
            Some(serde_json::json!({ "cell_id": "cell-a" })),
        );
        assert!(
            missing_required.contains("Interrupted"),
            "canvas=\n{missing_required}"
        );

        // Unknown key on a strict object. (ExitPlanMode would NOT reject this:
        // its schema is `strictObject({...}).passthrough()`,
        // ExitPlanModeV2Tool.ts:77-88, so passthrough wins there.)
        let unknown_key = render_reject(
            "NotebookEdit",
            Some(serde_json::json!({
                "notebook_path": "/tmp/nb.ipynb",
                "new_source": "print(1)",
                "bogus": true
            })),
        );
        assert!(
            unknown_key.contains("Interrupted"),
            "canvas=\n{unknown_key}"
        );

        // Absent input parses like `safeParse(undefined)` — a strict object
        // rejects it, so the whole row falls back.
        let no_input = render_reject("AskUserQuestion", None);
        assert!(no_input.contains("Interrupted"), "canvas=\n{no_input}");
    }

    #[test]
    fn valid_inputs_still_reach_the_per_tool_renderers() {
        let notebook = render_reject(
            "NotebookEdit",
            Some(serde_json::json!({
                "notebook_path": "/tmp/nb.ipynb",
                "new_source": "print(1)"
            })),
        );
        assert!(!notebook.contains("Interrupted"), "canvas=\n{notebook}");

        // StructuredOutput's tool-object schema is `z.object({}).passthrough()`
        // (SyntheticOutputTool.ts:11): any object passes the gate.
        let synthetic = render_reject(
            "StructuredOutput",
            Some(serde_json::json!({ "anything": {"goes": true} })),
        );
        assert!(!synthetic.contains("Interrupted"), "canvas=\n{synthetic}");
    }
}
