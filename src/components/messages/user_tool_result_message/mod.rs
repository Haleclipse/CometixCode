//! Maps to: CC `components/messages/UserToolResultMessage/*`.
//! Official Claude Code does not render a generic "✓ Tool completed" row for
//! tool_result blocks. The visible chrome belongs to the originating tool's
//! result renderer (Bash output, Read snippet, Edit diff, etc.). This Rust port
//! keeps that split at the UserToolResultMessage boundary and degrades to
//! content only when no typed renderer data is available.

pub mod rejected_plan_message;
pub mod rejected_tool_use_message;
pub mod user_tool_canceled_message;
pub mod user_tool_error_message;
pub mod user_tool_reject_message;
pub mod user_tool_success_message;
pub mod utils;

use self::user_tool_canceled_message::UserToolCanceledMessage;
use self::user_tool_error_message::UserToolErrorMessage;
use self::user_tool_reject_message::UserToolRejectMessage;
use self::user_tool_success_message::UserToolSuccessMessage;
use self::utils::{ToolRenderBackground, ToolRenderLine, ToolRenderOptions, ToolRenderTone};
use crate::components::file_edit_tool_updated_message::FileEditToolUpdatedMessage;
use crate::components::file_edit_tool_use_rejected_message::FileEditToolUseRejectedMessage;
use crate::components::notebook_edit_tool_use_rejected_message::NotebookEditToolUseRejectedMessage;
use crate::tools::{
    ask_user_question_tool, bash_tool, code_search_tool, enter_plan_mode_tool, enter_worktree_tool,
    exit_plan_mode_tool, exit_worktree_tool, file_edit_tool, file_read_tool, file_write_tool,
    glob_tool, grep_tool, list_mcp_resources_tool, lsp_tool, mcp_tool, notebook_edit_tool,
    powershell_tool, read_mcp_resource_tool, schedule_cron_tool, send_message_tool, skill_tool,
    synthetic_output_tool, task_output_tool, task_stop_tool, web_fetch_tool, web_search_tool,
};
use crate::types::message::{ReadResultKind, ToolResultStatus};
use crate::utils::truncate::truncate;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct UserToolResultMessageProps {
    pub tool_name: String,
    /// CC `ToolResultBlockParam.is_error` — with the content-prefix checks
    /// above it, this fully determines the leaf (UserToolResultMessage.tsx:
    /// cancel → reject → is_error → success). No stored status.
    pub is_error: bool,
    pub content: String,
    /// Maps to: CC `message.toolUseResult`, forwarded to the success leaf —
    /// the error leaf never reads it (`UserToolSuccessMessage.tsx:80` is the
    /// only consumer among the leaves).
    pub tool_use_result: Option<serde_json::Value>,
    /// Maps to: CC's `input` resolved from the paired tool_use in the lookups
    /// (`UserToolRejectMessage.tsx:14`; the success leaf forwards it as
    /// `options.input`, `UserToolSuccessMessage.tsx:97`).
    pub tool_input: Option<serde_json::Value>,
    /// Maps to: CC `progressMessagesForMessage` — resolved from
    /// `lookups.progress_messages_by_tool_use_id` at the mount site; the
    /// Agent transcript block consumes it.
    pub progress_messages: Vec<crate::types::message::ToolUseProgressMessage>,
    pub verbose: bool,
    pub is_transcript_mode: bool,
    /// Maps to: CC `style?: 'condensed'` (MessageComponent → leaf →
    /// renderToolResultMessage / renderToolUseRejectedMessage). Set by the
    /// collapsed subagent view (AgentTool/UI.tsx:705).
    pub style: Option<String>,
}

/// Maps to: CC
/// `components/messages/UserToolResultMessage/UserToolResultMessage.tsx:32-101`
/// `UserToolResultMessage`.
#[component]
pub fn UserToolResultMessage(props: &UserToolResultMessageProps) -> impl Into<AnyElement<'static>> {
    // CC `UserToolResultMessage.tsx:43-46`: `useGetToolFromMessages` misses
    // for an orphan tool_result (no originating tool_use in the transcript)
    // and the component returns null. The Rust call sites resolve the same
    // lookup and hand an empty `tool_name` on a miss (message.rs; CC
    // UserToolResultMessage.tsx:43).
    if props.tool_name.is_empty() {
        return element! { Fragment }.into_any();
    }
    // The upstream status/content routing below mirrors CC :48-100; each leaf
    // owns its corresponding rendering contract.
    if crate::utils::messages::is_tool_cancel_message(&props.content) {
        return element! { UserToolCanceledMessage }.into_any();
    }
    if crate::utils::messages::is_plain_tool_reject_message(&props.content) {
        return element! {
            UserToolRejectMessage(
                tool_name: props.tool_name.clone(),
                tool_input: props.tool_input.clone(),
                progress_messages: props.progress_messages.clone(),
                verbose: props.verbose,
                is_transcript_mode: props.is_transcript_mode,
                style: props.style.clone(),
            )
        }
        .into_any();
    }
    if props
        .content
        .starts_with(crate::utils::messages::REJECT_MESSAGE_WITH_REASON_PREFIX)
    {
        return element! {
            UserToolErrorMessage(
                tool_name: props.tool_name.clone(),
                content: props.content.clone(),
                progress_messages: props.progress_messages.clone(),
                verbose: props.verbose,
                is_transcript_mode: props.is_transcript_mode,
            )
        }
        .into_any();
    }

    // CC UserToolResultMessage.tsx:73-90: after the prefix checks, is_error
    // selects the error leaf and everything else is success.
    if props.is_error {
        element! {
            UserToolErrorMessage(
                tool_name: props.tool_name.clone(),
                content: props.content.clone(),
                progress_messages: props.progress_messages.clone(),
                verbose: props.verbose,
                is_transcript_mode: props.is_transcript_mode,
            )
        }
        .into_any()
    } else {
        element! {
            UserToolSuccessMessage(
                tool_name: props.tool_name.clone(),
                content: props.content.clone(),
                tool_use_result: props.tool_use_result.clone(),
                tool_input: props.tool_input.clone(),
                progress_messages: props.progress_messages.clone(),
                verbose: props.verbose,
                is_transcript_mode: props.is_transcript_mode,
                style: props.style.clone(),
            )
        }
        .into_any()
    }
}

/// Maps to official tool-specific component delegation in:
/// - `tools/FileEditTool/UI.tsx#renderToolResultMessage`
/// - `tools/FileEditTool/UI.tsx#renderToolUseRejectedMessage`
/// - `tools/NotebookEditTool/UI.tsx#renderToolUseRejectedMessage`
/// Maps to: CC's per-tool element renderers for the file tools.
/// Success renders from the raw `toolUseResult` with the tool's own schema —
/// `FileEditTool/UI.tsx:89-108` and `FileWriteTool/UI.tsx:282-335` (update
/// arm); a missing or schema-rejected raw renders nothing
/// (`UserToolSuccessMessage.tsx:72,81`). Rejection renders from the paired
/// tool_use INPUT at render time, computing the preview diff on the spot —
/// `FileEditTool/UI.tsx:110-171` / `FileWriteTool/UI.tsx:138-150`. `None`
/// hands the row to the line pipeline (non-file tools, Write `create`
/// success, and input-less rejections falling to the fallback leaf).
/// By-tool-name dispatch to the file tools' element-pipeline renderers, the
/// stand-in for CC's `tool.renderToolResultMessage` /
/// `tool.renderToolUseRejectedMessage` dynamic dispatch. The render bodies
/// live with their defining owners (`file_edit_tool/ui.rs`,
/// `file_write_tool/ui.rs`); this shim only resolves the tool and parses the
/// raw `toolUseResult`.
fn official_file_result_element(
    tool_name: &str,
    status: ToolResultStatus,
    tool_use_result: Option<&serde_json::Value>,
    tool_input: Option<&serde_json::Value>,
    verbose: bool,
    style: Option<&str>,
) -> Option<AnyElement<'static>> {
    let is_edit = tool_name.eq_ignore_ascii_case("Edit")
        || tool_name.eq_ignore_ascii_case("MultiEdit")
        || tool_name.eq_ignore_ascii_case("FileEdit");
    let is_write =
        tool_name.eq_ignore_ascii_case("Write") || tool_name.eq_ignore_ascii_case("FileWrite");
    let is_notebook = tool_name.eq_ignore_ascii_case("NotebookEdit");
    if is_notebook {
        // CC `NotebookEditTool/UI.tsx:99-124` renderToolResultMessage; the
        // rejected leaf keeps its own input-driven component
        // (user_tool_reject_message.rs), so only success lands here.
        if status != ToolResultStatus::Success {
            return None;
        }
        let output =
            tool_use_result.and_then(crate::tools::notebook_edit_tool::ui::parse_output)?;
        return Some(
            crate::tools::notebook_edit_tool::ui::render_tool_result_message(&output, verbose),
        );
    }
    if !is_edit && !is_write {
        return None;
    }
    // CC's success leaf returns null when the raw output fails the tool's
    // schema (`UserToolSuccessMessage.tsx:72,81`).
    let empty = || Some(element! { View(width: 0u32, height: 0u32) }.into_any());

    match status {
        ToolResultStatus::Success if is_edit => {
            let Some(output) = tool_use_result.and_then(file_edit_tool::ui::parse_output) else {
                return empty();
            };
            Some(file_edit_tool::ui::render_tool_result_message(
                &output, verbose, style,
            ))
        }
        ToolResultStatus::Success => {
            let Some(output) = tool_use_result.and_then(file_write_tool::ui::parse_output) else {
                return empty();
            };
            file_write_tool::ui::render_tool_result_message(&output, verbose, style)
        }
        ToolResultStatus::Rejected if is_edit => {
            file_edit_tool::ui::render_tool_use_rejected_message(tool_input?, verbose, style)
        }
        ToolResultStatus::Rejected => {
            file_write_tool::ui::render_tool_use_rejected_message(tool_input?, verbose, style)
        }
        _ => None,
    }
}

pub fn render_tool_result_lines(
    tool_name: &str,
    status: ToolResultStatus,
    content: &str,
) -> Vec<ToolRenderLine> {
    render_tool_result_lines_with_options(tool_name, status, content, ToolRenderOptions::default())
}

/// The name→output-parser registry — CC has NO such function. CC hangs
/// `outputSchema` on each Tool object and the render layer resolves it by
/// name (`tool.outputSchema?.safeParse(message.toolUseResult)`,
/// `UserToolSuccessMessage.tsx:80`); Rust keeps the same knowledge in one
/// table instead of on the trait, because the render side resolves tools by
/// name rather than through the registry object. `Some` means the tool has a
/// strict output parser: a Success row whose raw is missing or fails that
/// parse renders nothing (`UserToolSuccessMessage.tsx:72,81`). Consumers: the
/// emit gate (`transcript_tool_result_should_emit_ui`) and the resume seed
/// projection in query.rs. The terminal shape would be the per-tool schema on
/// `ToolCall` itself.
pub(crate) fn migrated_output_parses(tool_name: &str) -> Option<fn(&serde_json::Value) -> bool> {
    let parses: fn(&serde_json::Value) -> bool = match tool_name.to_ascii_lowercase().as_str() {
        "read" => |raw| {
            crate::tools::file_read_tool::parse_output(
                &crate::tools::file_read_tool::javascript_runtime_value(raw),
            )
            .is_some()
        },
        "grep" => |raw| crate::tools::grep_tool::ui::parse_output(raw).is_some(),
        "glob" => |raw| crate::tools::glob_tool::ui::parse_output(raw).is_some(),
        "codesearch" => |raw| crate::tools::code_search_tool::parse_output(raw).is_some(),
        "notebookedit" => |raw| crate::tools::notebook_edit_tool::ui::parse_output(raw).is_some(),
        "webfetch" => |raw| crate::tools::web_fetch_tool::ui::parse_output(raw).is_some(),
        "websearch" => |raw| crate::tools::web_search_tool::ui::parse_output(raw).is_some(),
        "croncreate" => {
            |raw| crate::tools::schedule_cron_tool::ui::parse_create_output(raw).is_some()
        }
        "crondelete" => {
            |raw| crate::tools::schedule_cron_tool::ui::parse_delete_output(raw).is_some()
        }
        "cronlist" => |raw| crate::tools::schedule_cron_tool::ui::parse_list_output(raw).is_some(),
        "listmcpresources" | "listmcpresourcestool" => {
            |raw| crate::tools::list_mcp_resources_tool::ui::parse_output(raw).is_some()
        }
        "readmcpresource" | "readmcpresourcetool" => {
            |raw| crate::tools::read_mcp_resource_tool::ui::parse_output(raw).is_some()
        }
        "enterworktree" => |raw| crate::tools::enter_worktree_tool::ui::parse_output(raw).is_some(),
        "exitworktree" => |raw| crate::tools::exit_worktree_tool::ui::parse_output(raw).is_some(),
        // KillShell is TaskStopTool's deprecated alias (TaskStopTool.ts:44).
        "taskstop" | "killshell" => {
            |raw| crate::tools::task_stop_tool::ui::parse_output(raw).is_some()
        }
        "structuredoutput" => {
            |raw| crate::tools::synthetic_output_tool::ui::parse_output(raw).is_some()
        }
        "lsp" => |raw| crate::tools::lsp_tool::ui::parse_output(raw).is_some(),
        "askuserquestion" => {
            |raw| crate::tools::ask_user_question_tool::ui::parse_output(raw).is_some()
        }
        "enterplanmode" => {
            |raw| crate::tools::enter_plan_mode_tool::ui::parse_output(raw).is_some()
        }
        "exitplanmode" => |raw| crate::tools::exit_plan_mode_tool::ui::parse_output(raw).is_some(),
        "skill" => |raw| crate::tools::skill_tool::ui::parse_output(raw).is_some(),
        // TaskOutputTool (aliases BashOutputTool/AgentOutputTool) defines no
        // outputSchema — the only gate is the raw's presence.
        "taskoutput" | "bashoutput" | "agentoutput" | "taskoutputtool" | "bashoutputtool"
        | "agentoutputtool" => |_raw| true,
        // SendMessageTool defines no outputSchema — CC's
        // `tool.outputSchema?.safeParse` short-circuits, so the only gate is
        // the raw's presence (`UserToolSuccessMessage.tsx:72`).
        "sendmessage" => |_raw| true,
        "sendusermessage" | "brief" => {
            |raw| crate::tools::brief_tool::ui::parse_output(raw).is_some()
        }
        // AgentTool (legacy alias Task).
        "agent" | "task" => |raw| crate::tools::agent_tool::ui::parse_output(raw).is_some(),
        "bash" => |raw| crate::tools::bash_tool::ui::parse_output(raw).is_some(),
        "powershell" => |raw| crate::tools::powershell_tool::ui::parse_output(raw).is_some(),
        "edit" | "multiedit" | "fileedit" => {
            |raw| crate::tools::file_edit_tool::ui::parse_output(raw).is_some()
        }
        "write" | "filewrite" => {
            |raw| crate::tools::file_write_tool::ui::parse_output(raw).is_some()
        }
        // MCP dynamic tools: any present raw renders (see the deviation note
        // in `render_tool_result_lines_by_tool_name` — CC 2.1.88's
        // z.string() safeParse would null the recorded array shape).
        name if crate::services::mcp::utils::is_mcp_tool_name(name) => |_raw| true,
        _ => return None,
    };
    Some(parses)
}

/// Maps to: CC `progressMessagesForMessage.at(-1)?.data?.timeoutMs`
/// (`BashTool/UI.tsx:189-190`, `PowerShellTool/UI.tsx:118-119`) — a last
/// progress message of any other shape yields `undefined` there, `None` here.
fn shell_timeout_ms_from_progress(
    progress_messages: &[crate::types::message::ToolUseProgressMessage],
) -> Option<u64> {
    match progress_messages.last()? {
        crate::types::message::ToolUseProgressMessage::BashProgress { timeout_ms, .. } => {
            *timeout_ms
        }
        _ => None,
    }
}

/// Maps to: CC `UserToolSuccessMessage.tsx:86-96` — the tool found by name
/// renders its own result from the raw `toolUseResult`. `None` means the
/// caller falls through to the generic tail. The migrated set is
/// `migrated_output_parses`.
fn render_tool_result_lines_by_tool_name(
    tool_name: &str,
    status: ToolResultStatus,
    content: &str,
    tool_use_result: Option<&serde_json::Value>,
    tool_input: Option<&serde_json::Value>,
    progress_messages: &[crate::types::message::ToolUseProgressMessage],
    options: &ToolRenderOptions,
) -> Option<Vec<ToolRenderLine>> {
    // Edit/Write line-pipeline rows: the success element renders via
    // `official_file_result_element`; this branch serves the error leaf
    // (`FileEditTool/UI.tsx:173-210` / `FileWriteTool/UI.tsx:264-280` compact
    // copies) and the transcript-mode / Write-create line rendering, all from
    // the raw `toolUseResult`.
    {
        let is_edit = tool_name.eq_ignore_ascii_case("Edit")
            || tool_name.eq_ignore_ascii_case("MultiEdit")
            || tool_name.eq_ignore_ascii_case("FileEdit");
        let is_write =
            tool_name.eq_ignore_ascii_case("Write") || tool_name.eq_ignore_ascii_case("FileWrite");
        if is_edit {
            if status == ToolResultStatus::Error {
                return Some(file_edit_tool::ui::render_tool_result_lines(
                    None,
                    0,
                    0,
                    "update",
                    &[],
                    &[],
                    status,
                    content,
                    *options,
                ));
            }
            let Some(output) = tool_use_result.and_then(file_edit_tool::ui::parse_output) else {
                // Missing raw, or a shape the schema rejects (including the
                // EditError `toolUseResult` string), renders nothing
                // (`UserToolSuccessMessage.tsx:72,81`).
                return Some(Vec::new());
            };
            let diff_lines = output
                .structured_patch
                .iter()
                .flat_map(|hunk| hunk.lines.iter().cloned())
                .collect::<Vec<_>>();
            return Some(file_edit_tool::ui::render_tool_result_lines(
                Some(&output.file_path),
                crate::utils::diff::count_added_lines(&diff_lines),
                crate::utils::diff::count_removed_lines(&diff_lines),
                "update",
                &diff_lines,
                &output.structured_patch,
                status,
                content,
                *options,
            ));
        }
        if is_write {
            if status == ToolResultStatus::Error {
                return Some(file_write_tool::ui::render_tool_result_lines(
                    None,
                    None,
                    "update",
                    status,
                    content,
                    None,
                    &[],
                    &[],
                    *options,
                ));
            }
            let Some(output) = tool_use_result.and_then(file_write_tool::ui::parse_output) else {
                return Some(Vec::new());
            };
            let diff_lines = output
                .structured_patch
                .iter()
                .flat_map(|hunk| hunk.lines.iter().cloned())
                .collect::<Vec<_>>();
            let operation = match output.kind {
                crate::tools::file_write_tool::WriteOutputKind::Create => "create",
                crate::tools::file_write_tool::WriteOutputKind::Update => "update",
            };
            return Some(file_write_tool::ui::render_tool_result_lines(
                Some(&output.file_path),
                Some(crate::tools::file_write_tool::ui::count_lines(
                    &output.content,
                )),
                operation,
                status,
                content,
                Some(&output.content),
                &diff_lines,
                &output.structured_patch,
                *options,
            ));
        }
    }

    // Bash and PowerShell resolve `timeoutMs` from the last progress message
    // (`BashTool/UI.tsx:189-190`, `PowerShellTool/UI.tsx:118-119`); their
    // error leaf is `FallbackToolUseErrorMessage` (`UI.tsx:200` / `:168`).
    if tool_name.eq_ignore_ascii_case("Bash") || tool_name.eq_ignore_ascii_case("PowerShell") {
        if status == ToolResultStatus::Error {
            return Some(fallback_shell_error_lines(content, *options));
        }
        let Some(raw) = tool_use_result else {
            return Some(Vec::new());
        };
        let timeout_ms = shell_timeout_ms_from_progress(progress_messages);
        return Some(if tool_name.eq_ignore_ascii_case("Bash") {
            match bash_tool::ui::parse_output(raw) {
                Some(output) => {
                    bash_tool::ui::render_tool_result_message(&output, timeout_ms, *options)
                }
                None => Vec::new(),
            }
        } else {
            match powershell_tool::ui::parse_output(raw) {
                Some(output) => {
                    powershell_tool::ui::render_tool_result_message(&output, timeout_ms, *options)
                }
                None => Vec::new(),
            }
        });
    }
    // MCP dynamic tools (`mcp__server__tool`). No renderToolUseErrorMessage →
    // fallback. Nyan-specific deviation: CC 2.1.88's success leaf runs
    // `safeParse(z.string())` on the raw (UserToolSuccessMessage.tsx:80) and
    // rejects the array-of-content-blocks shape `mcpClient` actually records
    // (client.ts:1898), nulling every MCP result row — an upstream regression
    // of the #39817 defense; the renderer itself has an explicit
    // Array.isArray branch (MCPTool/UI.tsx:143). We keep the renderer's
    // intent: any present raw renders.
    if crate::services::mcp::utils::is_mcp_tool_name(tool_name) {
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        // A missing raw renders nothing (`UserToolSuccessMessage.tsx:72`).
        let Some(raw) = tool_use_result else {
            return Some(Vec::new());
        };
        return Some(mcp_tool::ui::render_tool_result_message(
            raw, tool_input, *options,
        ));
    }
    if tool_name.eq_ignore_ascii_case("Read") {
        // Unparseable Read output never reaches here — the caller
        // short-circuits it above, which is the same "render nothing" the raw
        // entry returns for output it cannot parse.

        // Maps to: CC `UserToolResultMessage.tsx` routing an `is_error` result
        // to the error leaf — the raw output plays no part there; the error
        // copy comes from the tool-result content.
        if status == ToolResultStatus::Error {
            return Some(
                match file_read_tool::ui::render_tool_use_error_message(content, options.verbose) {
                    Some(message) => vec![ToolRenderLine::new(message, ToolRenderTone::Error)],
                    None => fallback_tool_use_error_lines(content, options.verbose),
                },
            );
        }

        // The raw is CC's `message.toolUseResult`, handed in by the caller.
        // Unconditional, as CC is: a Read success row with no `toolUseResult`
        // renders nothing (`UserToolSuccessMessage.tsx:72` returns null before
        // it even reaches the tool).
        return Some(file_read_tool::ui::render_tool_result_message(
            tool_use_result,
            status,
            content,
        ));
    }
    if tool_name.eq_ignore_ascii_case("Grep") {
        // Maps to: CC `UserToolResultMessage.tsx` routing an `is_error` result
        // to the error leaf — `GrepTool/UI.tsx:100-124` short-circuits the
        // tagged copy, else `FallbackToolUseErrorMessage`.
        if status == ToolResultStatus::Error {
            return Some(
                match grep_tool::ui::search_tool_use_error_message(content, options.verbose) {
                    Some(message) => vec![ToolRenderLine::new(message, ToolRenderTone::Error)],
                    None => fallback_tool_use_error_lines(content, options.verbose),
                },
            );
        }

        // Success leaf: parse the raw `toolUseResult` with the tool's own
        // output schema and render `SearchResultSummary` from it
        // (`UserToolSuccessMessage.tsx:80-96` → `GrepTool/UI.tsx:126-172`).
        return Some(grep_tool::ui::render_tool_result_message(
            tool_use_result,
            status,
            content,
            options,
        ));
    }
    if tool_name.eq_ignore_ascii_case("Glob") {
        // Maps to: CC `UserToolResultMessage.tsx` error leaf —
        // `GlobTool/UI.tsx:29-53` short-circuits the tagged copy, else
        // `FallbackToolUseErrorMessage`.
        if status == ToolResultStatus::Error {
            return Some(
                match glob_tool::ui::render_tool_use_error_message(content, options.verbose) {
                    Some(message) => vec![ToolRenderLine::new(message, ToolRenderTone::Error)],
                    None => fallback_tool_use_error_lines(content, options.verbose),
                },
            );
        }

        // Success leaf: Glob parses with its own schema and reuses Grep's
        // renderer (`GlobTool/UI.tsx:56`).
        return Some(glob_tool::ui::render_tool_result_message(
            tool_use_result,
            status,
            content,
            options,
        ));
    }
    if tool_name.eq_ignore_ascii_case("CodeSearch") {
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        let Some(output) = tool_use_result.and_then(code_search_tool::parse_output) else {
            return Some(Vec::new());
        };
        return Some(vec![ToolRenderLine::new(
            output.content,
            ToolRenderTone::Normal,
        )]);
    }
    if tool_name.eq_ignore_ascii_case("EnterWorktree")
        || tool_name.eq_ignore_ascii_case("ExitWorktree")
    {
        // Neither tool defines renderToolUseErrorMessage, so the error leaf
        // is FallbackToolUseErrorMessage directly.
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(if tool_name.eq_ignore_ascii_case("EnterWorktree") {
            enter_worktree_tool::ui::render_tool_result_message(tool_use_result)
        } else {
            exit_worktree_tool::ui::render_tool_result_message(tool_use_result)
        });
    }
    if tool_name.eq_ignore_ascii_case("ListMcpResources")
        || tool_name.eq_ignore_ascii_case("ListMcpResourcesTool")
        || tool_name.eq_ignore_ascii_case("ReadMcpResource")
        || tool_name.eq_ignore_ascii_case("ReadMcpResourceTool")
    {
        // Neither tool defines renderToolUseErrorMessage, so the error leaf
        // is FallbackToolUseErrorMessage directly.
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(
            if tool_name
                .to_ascii_lowercase()
                .starts_with("listmcpresources")
            {
                list_mcp_resources_tool::ui::render_tool_result_message(tool_use_result)
            } else {
                read_mcp_resource_tool::ui::render_tool_result_message(tool_use_result)
            },
        );
    }
    if tool_name.eq_ignore_ascii_case("CronCreate")
        || tool_name.eq_ignore_ascii_case("CronDelete")
        || tool_name.eq_ignore_ascii_case("CronList")
    {
        // None of the cron tools define renderToolUseErrorMessage, so the
        // error leaf is FallbackToolUseErrorMessage directly.
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(if tool_name.eq_ignore_ascii_case("CronCreate") {
            schedule_cron_tool::ui::render_create_result_message(tool_use_result)
        } else if tool_name.eq_ignore_ascii_case("CronDelete") {
            schedule_cron_tool::ui::render_delete_result_message(tool_use_result)
        } else {
            schedule_cron_tool::ui::render_list_result_message(tool_use_result)
        });
    }
    if tool_name.eq_ignore_ascii_case("WebFetch") || tool_name.eq_ignore_ascii_case("WebSearch") {
        // Neither tool defines renderToolUseErrorMessage, so the error leaf
        // is FallbackToolUseErrorMessage directly.
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        let render = if tool_name.eq_ignore_ascii_case("WebFetch") {
            web_fetch_tool::ui::render_tool_result_message
        } else {
            web_search_tool::ui::render_tool_result_message
        };
        return Some(render(tool_use_result, status, content, options));
    }
    if tool_name.eq_ignore_ascii_case("TaskStop") || tool_name.eq_ignore_ascii_case("KillShell") {
        // TaskStopTool defines no renderToolUseErrorMessage, so the error
        // leaf is FallbackToolUseErrorMessage directly.
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(task_stop_tool::ui::render_tool_result_message(
            tool_use_result,
            options.verbose,
        ));
    }
    if tool_name.eq_ignore_ascii_case("StructuredOutput") {
        // `renderToolUseErrorMessage` is the bare constant string
        // (`SyntheticOutputTool.ts:85-87`) — React renders the naked return
        // with no color.
        if status == ToolResultStatus::Error {
            return Some(vec![ToolRenderLine::new(
                synthetic_output_tool::ui::render_error_message(),
                ToolRenderTone::Normal,
            )]);
        }
        return Some(synthetic_output_tool::ui::render_tool_result_message(
            tool_use_result,
        ));
    }
    // AskUserQuestion's rejected leaf renders from the reject dispatch
    // (`renderToolUseRejectedMessage`); its error renderer returns null,
    // which `?? FallbackToolUseErrorMessage` resolves to the fallback.
    if tool_name.eq_ignore_ascii_case("AskUserQuestion") && status != ToolResultStatus::Rejected {
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(ask_user_question_tool::ui::render_tool_result_message(
            tool_use_result,
        ));
    }
    // EnterPlanMode's rejected leaf also renders from the reject dispatch;
    // it defines no renderToolUseErrorMessage.
    if tool_name.eq_ignore_ascii_case("EnterPlanMode") && status != ToolResultStatus::Rejected {
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(enter_plan_mode_tool::ui::render_tool_result_message(
            tool_use_result,
        ));
    }
    // ExitPlanMode's rejected leaf renders from the reject dispatch (input
    // plan ?? getPlan() ?? 'No plan found'); PLAN_REJECTION_PREFIX error
    // rows are already routed by the central error leaf before this
    // dispatch. No renderToolUseErrorMessage → fallback.
    if tool_name.eq_ignore_ascii_case("ExitPlanMode") && status != ToolResultStatus::Rejected {
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(exit_plan_mode_tool::ui::render_tool_result_message(
            tool_use_result,
        ));
    }
    // AgentTool (legacy alias Task). CC's error/rejected renderers replay
    // progress and then the fallback (`AgentTool/UI.tsx:723-789`). The
    // component path carries that replay: the error/reject leaves dispatch to
    // `agent_tool::ui::render_tool_use_error_message_element` /
    // `render_tool_use_rejected_message_element`. This line pipeline has no
    // progress channel, so its error arm stays the bare fallback for the
    // plain-text consumers.
    if tool_name.eq_ignore_ascii_case("Agent") || tool_name.eq_ignore_ascii_case("Task") {
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        let Some(output) = tool_use_result.and_then(crate::tools::agent_tool::ui::parse_output)
        else {
            return Some(Vec::new());
        };
        return Some(crate::tools::agent_tool::ui::render_tool_result_lines(
            &output.status,
            output.agent_id.as_deref(),
            output.prompt.as_deref(),
            &output.content,
            output.total_tool_use_count,
            output.total_duration_ms,
            output.total_tokens,
            *options,
        ));
    }
    // CC's BashOutputTool / AgentOutputTool are aliases of TaskOutputTool
    // (TaskOutputTool.tsx:176). Its renderToolUseErrorMessage
    // (TaskOutputTool.tsx:417-419) is defined but its body IS
    // FallbackToolUseErrorMessage, so the fallback here is behaviorally 1:1.
    if tool_name.eq_ignore_ascii_case("TaskOutput")
        || tool_name.eq_ignore_ascii_case("BashOutput")
        || tool_name.eq_ignore_ascii_case("AgentOutput")
        || tool_name.eq_ignore_ascii_case("TaskOutputTool")
        || tool_name.eq_ignore_ascii_case("BashOutputTool")
        || tool_name.eq_ignore_ascii_case("AgentOutputTool")
    {
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(task_output_tool::ui::render_tool_result_message(
            tool_use_result,
            content,
            status,
            *options,
        ));
    }
    if tool_name.eq_ignore_ascii_case("Skill") {
        // CC's Skill error/rejected renderers replay progress messages and
        // then the fallback (`SkillTool/UI.tsx:137-181`); the progress replay
        // lives outside the line pipeline, so the error leaf falls back
        // directly here.
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(skill_tool::ui::render_tool_result_message(tool_use_result));
    }
    // SendUserMessage (legacy alias Brief) — the line-pipeline projection of
    // the component-level BriefToolResultMessage: the message text, or
    // nothing when the renderer returns null (`BriefTool/UI.tsx:24-27`).
    // No renderToolUseErrorMessage → fallback.
    if tool_name.eq_ignore_ascii_case("SendUserMessage") || tool_name.eq_ignore_ascii_case("Brief")
    {
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        let Some(output) = tool_use_result.and_then(crate::tools::brief_tool::ui::parse_output)
        else {
            return Some(Vec::new());
        };
        let has_attachments = output
            .attachments
            .as_deref()
            .is_some_and(|attachments| !attachments.is_empty());
        if output.message.is_empty() && !has_attachments {
            return Some(Vec::new());
        }
        return Some(vec![ToolRenderLine::new(
            output.message,
            ToolRenderTone::Normal,
        )]);
    }
    if tool_name.eq_ignore_ascii_case("SendMessage") {
        // SendMessageTool defines no renderToolUseErrorMessage, so the error
        // leaf is FallbackToolUseErrorMessage directly.
        if status == ToolResultStatus::Error {
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }
        return Some(send_message_tool::ui::render_tool_result_message(
            tool_use_result,
        ));
    }
    if tool_name.eq_ignore_ascii_case("LSP") {
        // Maps to: CC `UserToolResultMessage.tsx` error leaf —
        // `LSPTool/UI.tsx:160-176` compacts tagged copy to "LSP operation
        // failed" when not verbose, else `FallbackToolUseErrorMessage`.
        if status == ToolResultStatus::Error {
            return Some(
                match lsp_tool::ui::render_tool_use_error_message(content, options.verbose) {
                    Some(message) => vec![ToolRenderLine::new(message, ToolRenderTone::Error)],
                    None => fallback_tool_use_error_lines(content, options.verbose),
                },
            );
        }
        return Some(lsp_tool::ui::render_tool_result_message(
            tool_use_result,
            options.verbose,
        ));
    }
    // NotebookEdit's rejected leaf renders from the tool INPUT
    // (`renderToolUseRejectedMessage`, UI.tsx:56-79), so a Rejected row falls
    // through to its input-driven display variant below.
    if tool_name.eq_ignore_ascii_case("NotebookEdit") && status != ToolResultStatus::Rejected {
        // Maps to: CC `UserToolResultMessage.tsx` error leaf —
        // `NotebookEditTool/UI.tsx:81-97` compacts tagged copy to "Error
        // editing notebook", else `FallbackToolUseErrorMessage`.
        if status == ToolResultStatus::Error {
            if !options.verbose
                && crate::utils::messages::extract_tag(content, "tool_use_error").is_some()
            {
                return Some(vec![ToolRenderLine::new(
                    "Error editing notebook",
                    ToolRenderTone::Error,
                )]);
            }
            return Some(fallback_tool_use_error_lines(content, options.verbose));
        }

        // Success leaf: parse the raw `toolUseResult` with the tool's own
        // output schema (`UserToolSuccessMessage.tsx:80-96` →
        // `NotebookEditTool/UI.tsx:99-127`).
        return Some(notebook_edit_tool::ui::render_tool_result_lines(
            tool_use_result,
            status,
            content,
            options,
        ));
    }
    None
}

pub fn render_tool_result_lines_with_options(
    tool_name: &str,
    status: ToolResultStatus,
    content: &str,
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    render_tool_result_lines_for_result(tool_name, status, content, None, None, &[], options)
}

/// The full entry — what CC's render path receives: the tool resolved by
/// name, the raw `message.toolUseResult`, the paired tool_use input, and the
/// progress messages (`UserToolResultMessage.tsx:43`,
/// `UserToolSuccessMessage.tsx:72-103`). Callers holding a `ToolResult` pass
/// `result.tool_use_result.as_ref()`.
pub fn render_tool_result_lines_for_result(
    tool_name: &str,
    status: ToolResultStatus,
    content: &str,
    tool_use_result: Option<&serde_json::Value>,
    tool_input: Option<&serde_json::Value>,
    progress_messages: &[crate::types::message::ToolUseProgressMessage],
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    // Maps to: CC `UserToolErrorMessage.tsx:52-59` — the central error leaf
    // routes PLAN_REJECTION_PREFIX content to `RejectedPlanMessage` before
    // any per-tool error renderer, for every tool. CC substrings without
    // trimming.
    if status == ToolResultStatus::Error
        && content.starts_with(crate::utils::messages::PLAN_REJECTION_PREFIX)
    {
        return exit_plan_mode_tool::ui::render_rejected_result_lines(
            &content[crate::utils::messages::PLAN_REJECTION_PREFIX.len()..],
        );
    }

    // Dispatch by tool name, as CC does — `UserToolSuccessMessage.tsx:80-96`
    // looks the tool up by name and hands it the raw `toolUseResult`.
    if let Some(lines) = render_tool_result_lines_by_tool_name(
        tool_name,
        status,
        content,
        tool_use_result,
        tool_input,
        progress_messages,
        &options,
    ) {
        return lines;
    }

    generic_lines(tool_name, status, content)
}

fn generic_lines(tool_name: &str, status: ToolResultStatus, content: &str) -> Vec<ToolRenderLine> {
    let text = if content.trim().is_empty() {
        match status {
            ToolResultStatus::Canceled => "Interrupted by user".to_string(),
            ToolResultStatus::Rejected => "Tool use rejected".to_string(),
            ToolResultStatus::Success => format!("{tool_name} completed"),
            ToolResultStatus::Error => format!("{tool_name} failed"),
        }
    } else {
        content.trim().to_string()
    };
    vec![ToolRenderLine::new(text, status_tone(status))]
}

// Pure-row iocraft carrier for CC `FallbackToolUseErrorMessage`. Error
// normalization and truncation stay in the canonical fallback owner; this
// generic result owner only converts its render data for non-component users.
fn fallback_tool_use_error_lines(content: &str, verbose: bool) -> Vec<ToolRenderLine> {
    // Owner: the component's own line-pipeline projection.
    crate::components::fallback_tool_use_error_message::fallback_tool_use_error_lines(
        content, verbose,
    )
}

/// Maps to: CC Bash/PowerShell `renderToolUseErrorMessage` →
/// `FallbackToolUseErrorMessage` (10 raw lines; verbose shows all).
fn fallback_shell_error_lines(content: &str, options: ToolRenderOptions) -> Vec<ToolRenderLine> {
    let render = crate::components::fallback_tool_use_error_message::fallback_tool_use_error_render(
        Some(content),
        options.show_full(),
    );
    let mut lines = vec![ToolRenderLine::new(
        render.visible_error,
        ToolRenderTone::Error,
    )];
    if render.hidden_lines > 0 {
        lines.push(ToolRenderLine::new(
            format!(
                "… +{} {} (ctrl+o to see all)",
                render.hidden_lines,
                if render.hidden_lines == 1 {
                    "line"
                } else {
                    "lines"
                }
            ),
            ToolRenderTone::Inactive,
        ));
    }
    lines
}

fn status_tone(status: ToolResultStatus) -> ToolRenderTone {
    // Generic success fallbacks use default text color (official has no
    // `color="success"` on `{tool} completed`). Keep Success only for Agent
    // Prompt:/Response: labels which officially use `color="success"` — those
    // labels are emitted by `agent_tool::ui::push_agent_prompt_lines` /
    // `push_agent_response_lines`, not from here.
    match status {
        ToolResultStatus::Success => ToolRenderTone::Normal,
        ToolResultStatus::Error => ToolRenderTone::Error,
        ToolResultStatus::Rejected => ToolRenderTone::Warning,
        ToolResultStatus::Canceled => ToolRenderTone::Inactive,
    }
}

fn contains_ansi_escape(content: &str) -> bool {
    content.contains("\x1b[") || content.contains("\x1b]")
}

// ─── Recorded tool-result routing ─────────────────────────────────────────
// Maps to: CC `components/messages/UserToolResultMessage/UserToolResultMessage.tsx`
// (:32) content-prefix routing (CANCEL_MESSAGE / REJECT_MESSAGE /
// INTERRUPT_MESSAGE_FOR_TOOL_USE / is_error) plus the Success/Error/Reject
// leaf components (`UserToolSuccessMessage.tsx`:38, `UserToolErrorMessage.tsx`:33,
// `UserToolRejectMessage.tsx`:24).

pub(crate) fn success_tool_result_is_nonvisual(
    tool_name: &str,
    tool_use_result: Option<&serde_json::Value>,
) -> bool {
    let lower = tool_name.to_ascii_lowercase();
    match lower.as_str() {
        "taskcreate" => !crate::tools::task_create_tool::ui::renders_success_result(),
        "taskupdate" => !crate::tools::task_update_tool::ui::renders_success_result(),
        "tasklist" => !crate::tools::task_list_tool::ui::renders_success_result(),
        "taskget" => !crate::tools::task_get_tool::ui::renders_success_result(),
        "teamcreate" => !crate::tools::team_create_tool::ui::renders_success_result(),
        "teamdelete" => {
            !crate::tools::team_delete_tool::ui::renders_success_result(true, true, true)
        }
        "sendmessage" => tool_use_result
            .and_then(crate::tools::send_message_tool::ui::parse_result)
            .is_some_and(|view| !crate::tools::send_message_tool::ui::renders_result(&view)),
        // `createMcpAuthTool` defines no renderToolResultMessage. Cold replay
        // has the persisted registry name/output rather than the live Tool
        // object, so retain only the exact generated-name plus output-status
        // projection; ordinary MCP tools named `authenticate` are unaffected.
        _ if lower.starts_with("mcp__") && lower.ends_with("__authenticate") => tool_use_result
            .and_then(|result| result.get("status"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|status| matches!(status, "auth_url" | "unsupported" | "error")),
        // CC ToolSearchTool / TodoWriteTool: no renderToolResultMessage → null UI.
        "todowrite" | "toolsearch" => true,
        _ => false,
    }
}

/// Whether a live `QueryEvent::Row` tool_result row should be pushed to the
/// transcript UI. Model `tool_result` history is unaffected.
/// Maps to: CC rendering null for a success row whose tool defines no
/// `renderToolResultMessage` (`UserToolSuccessMessage.tsx:86-103`) or whose
/// raw `toolUseResult` is missing / schema-rejected (`:72,81`). CC leaves the
/// message in the list and lets React render nothing; this Rust seam drops
/// the row from the render list — visually identical, and it keeps the
/// row-count/index space the iocraft list depends on.
pub(crate) fn transcript_tool_result_should_emit_ui(
    message: &crate::types::message::RenderableMessage,
    tool_name: Option<&str>,
) -> bool {
    use crate::types::message::RenderableMessageKind;
    match &message.kind {
        RenderableMessageKind::User { message } => match message.first_content_block() {
            Some(crate::types::message::UserContent::ToolResult(tool_result)) => {
                // CC's lookup is two-layered and runs BEFORE any status
                // dispatch (`UserToolResultMessage/utils.tsx:12-19`): the
                // paired tool_use must exist AND `findToolByName` must
                // resolve the name against the session tools; either miss
                // renders the whole row null — error rows included
                // (`UserToolResultMessage.tsx:43-46`). Rows carry no tool
                // name, so the pairing miss arrives here as `None`.
                let Some(tool_name) = tool_name else {
                    return false;
                };
                if !tool_name_resolves(tool_name) {
                    return false;
                }
                if tool_result.derived_status() != ToolResultStatus::Success {
                    return true;
                }
                let raw = tool_result.tool_use_result.as_ref();
                if success_tool_result_is_nonvisual(tool_name, raw) {
                    return false;
                }
                if let Some(parses) = migrated_output_parses(tool_name) {
                    return matches!(raw, Some(raw) if parses(raw));
                }
                true
            }
            _ => true,
        },
        _ => true,
    }
}

/// The second lookup layer — CC `findToolByName(tools, name)` matching the
/// primary name or an alias (`Tool.ts:349-361`). The Rust registry stands in
/// for the session tools array. Declared deviation: any `mcp__*` name
/// resolves, because CC's array holds only the FETCHED dynamic MCP tools and
/// the render layer here has no fetch snapshot — an offline server's tool
/// name renders where CC would null it.
pub(crate) fn tool_name_resolves(tool_name: &str) -> bool {
    crate::services::tools::tool_execution::find_tool_call(tool_name).is_some()
        || crate::services::mcp::utils::is_mcp_tool_name(tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_read_success_keeps_raw_output_while_hiding_the_row() {
        // Read output the strict parser rejects: the raw stays on the row for
        // round trips, while the emit gate hides the row exactly as CC's
        // success leaf null-renders a `safeParse` failure
        // (`UserToolSuccessMessage.tsx:80-83`).
        let raw_output = serde_json::json!({
            "type": "text",
            "file": {"filePath": "src/lib.rs", "content": "content", "numLines": 1}
        });
        assert!(crate::tools::file_read_tool::parse_output(&raw_output).is_none());

        let tool_result = crate::types::message::ToolResult {
            tool_use_id: crate::types::ids::ToolUseId("toolu-read-malformed".to_string()),
            content: "1\tcontent".to_string(),
            is_error: false,
            content_blocks: Vec::new(),
            tool_use_result: Some(raw_output.clone()),
        };
        assert_eq!(tool_result.tool_use_result.as_ref(), Some(&raw_output));
        let message = crate::types::message::RenderableMessage::user_block(
            "malformed-read-result",
            crate::types::message::UserContent::ToolResult(tool_result),
        );
        assert!(!transcript_tool_result_should_emit_ui(
            &message,
            Some("Read")
        ));
    }

    #[test]
    fn tool_search_error_rows_pass_the_emit_ui_gate_like_official() {
        // CC ToolSearchTool defines neither renderToolResultMessage nor
        // renderToolUseErrorMessage, so a failing result still renders through
        // `UserToolErrorMessage.tsx:85-94`'s generic
        // FallbackToolUseErrorMessage. The emit-UI gate therefore only drops
        // *success* rows — an is_error row must pass.
        let tool_result = crate::types::message::ToolResult {
            tool_use_id: crate::types::ids::ToolUseId("toolu-tool-search-error".to_string()),
            content:
                "<tool_use_error>ToolSearch returned an unexpected output variant</tool_use_error>"
                    .to_string(),
            is_error: true,
            content_blocks: Vec::new(),
            tool_use_result: None,
        };
        assert_eq!(tool_result.derived_status(), ToolResultStatus::Error);
        let message = crate::types::message::RenderableMessage::user_block(
            "tool-search-error-row",
            crate::types::message::UserContent::ToolResult(tool_result),
        );
        assert!(transcript_tool_result_should_emit_ui(
            &message,
            Some("ToolSearch")
        ));
    }

    /// Maps to: CC `UserToolResultMessage/utils.tsx:12-19` — the lookup is
    /// two-layered and precedes status dispatch: a pairing miss OR a
    /// `findToolByName` miss nulls the whole row, error rows included
    /// (`UserToolResultMessage.tsx:43-46`).
    #[test]
    fn registry_miss_hides_the_row_for_every_status_like_official() {
        let row = |is_error: bool| {
            crate::types::message::RenderableMessage::user_block(
                "registry-miss-row",
                crate::types::message::UserContent::ToolResult(crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId("toolu-x".to_string()),
                    content: if is_error {
                        "<tool_use_error>boom</tool_use_error>".to_string()
                    } else {
                        "ok".to_string()
                    },
                    is_error,
                    content_blocks: Vec::new(),
                    tool_use_result: None,
                }),
            )
        };

        // Layer 1: pairing miss (no tool name resolved from the lookups).
        assert!(!transcript_tool_result_should_emit_ui(&row(false), None));
        assert!(!transcript_tool_result_should_emit_ui(&row(true), None));

        // Layer 2: the name pairs but resolves no tool — a retired tool from
        // an old transcript. CC nulls success AND error rows alike.
        assert!(!transcript_tool_result_should_emit_ui(
            &row(false),
            Some("RetiredTool")
        ));
        assert!(!transcript_tool_result_should_emit_ui(
            &row(true),
            Some("RetiredTool")
        ));

        // Aliases resolve like primary names (Tool.ts:349-361), and any
        // mcp__* name resolves (declared deviation: no fetch snapshot at
        // render, so offline servers' names pass where CC would null).
        assert!(transcript_tool_result_should_emit_ui(
            &row(true),
            Some("KillShell")
        ));
        assert!(transcript_tool_result_should_emit_ui(
            &row(true),
            Some("mcp__linear__list_issues")
        ));
    }

    /// Maps to: CC `UserToolSuccessMessage.tsx:86-103` — a tool whose
    /// `renderToolResultMessage` is absent, or returns null for this output,
    /// renders nothing on success.
    #[test]
    fn nonvisual_success_names_match_official_null_renderers() {
        // No renderToolResultMessage at all (TodoWriteTool.ts, ToolSearchTool.ts).
        assert!(success_tool_result_is_nonvisual("TodoWrite", None));
        assert!(success_tool_result_is_nonvisual("ToolSearch", None));
        // Case-insensitive, like every other by-name resolution here.
        assert!(success_tool_result_is_nonvisual("todowrite", None));

        // Tools whose UI owner decides: the answer comes from that owner, so
        // the table cannot drift away from it.
        assert_eq!(
            success_tool_result_is_nonvisual("TaskCreate", None),
            !crate::tools::task_create_tool::ui::renders_success_result()
        );
        assert_eq!(
            success_tool_result_is_nonvisual("TeamDelete", None),
            !crate::tools::team_delete_tool::ui::renders_success_result(true, true, true)
        );

        // SendMessage keys on the output shape (`SendMessageTool/UI.tsx:27-38`):
        // a routed result renders null, a plain message renders.
        let routed = serde_json::json!({
            "success": true,
            "message": "queued",
            "routing": {"kind": "team"}
        });
        let plain = serde_json::json!({"success": true, "message": "delivered"});
        assert!(success_tool_result_is_nonvisual(
            "SendMessage",
            Some(&routed)
        ));
        assert!(!success_tool_result_is_nonvisual(
            "SendMessage",
            Some(&plain)
        ));

        // `createMcpAuthTool` defines no renderToolResultMessage; the exact
        // generated name plus a non-success status is the cold-replay
        // projection of that fact.
        let auth_url = serde_json::json!({"status": "auth_url", "message": "open this"});
        assert!(success_tool_result_is_nonvisual(
            "mcp__linear__authenticate",
            Some(&auth_url)
        ));
        // An ordinary MCP tool that happens to be called `authenticate` is
        // unaffected, and so is a connected auth result.
        assert!(!success_tool_result_is_nonvisual(
            "mcp__linear__authenticate",
            Some(&serde_json::json!({"status": "connected"}))
        ));
        assert!(!success_tool_result_is_nonvisual(
            "mcp__linear__authenticate_user",
            Some(&auth_url)
        ));

        // Everything else renders.
        assert!(!success_tool_result_is_nonvisual("Read", None));
        assert!(!success_tool_result_is_nonvisual("UnknownTool", None));
    }

    /// Maps to: CC hanging `outputSchema` on each Tool and the render layer
    /// resolving it by name (`UserToolSuccessMessage.tsx:80`). Every name in
    /// the table must reach a parser that accepts that tool's real output and
    /// rejects a malformed one.
    #[test]
    fn migrated_output_parsers_accept_official_shapes_and_reject_malformed() {
        let read_ok = serde_json::json!({
            "type": "text",
            "file": {
                "filePath": "src/lib.rs",
                "content": "line",
                "numLines": 1,
                "startLine": 1,
                "totalLines": 1
            }
        });
        let read_bad = serde_json::json!({"type": "text", "file": {"filePath": "src/lib.rs"}});
        let parses = migrated_output_parses("Read").expect("Read is in the table");
        assert!(parses(&read_ok));
        assert!(!parses(&read_bad));

        // Aliases resolve to the same parser as their canonical name
        // (Task/Agent, KillShell/TaskStop, Brief/SendUserMessage,
        // BashOutput/TaskOutput).
        for (alias, canonical) in [
            ("Task", "Agent"),
            ("KillShell", "TaskStop"),
            ("Brief", "SendUserMessage"),
            ("BashOutput", "TaskOutput"),
        ] {
            assert!(
                migrated_output_parses(alias).is_some(),
                "alias {alias} must resolve"
            );
            assert!(
                migrated_output_parses(canonical).is_some(),
                "canonical {canonical} must resolve"
            );
        }

        // Any `mcp__*` name resolves through the MCP arm.
        assert!(migrated_output_parses("mcp__linear__list_issues").is_some());

        // A name with no parser falls through to the generic tail rather than
        // being gated.
        assert!(migrated_output_parses("NoSuchTool").is_none());
    }

    /// Maps to: CC's file-tool renderers — success reads
    /// `message.toolUseResult` (`FileEditTool/UI.tsx:66-108`,
    /// `FileWriteTool/UI.tsx:232-262`), rejection computes its preview from
    /// the tool INPUT (`:110-171`, `:138-150`). Only these tools claim the
    /// element path; everything else falls through to the line pipeline.
    #[test]
    fn official_file_result_element_claims_only_the_file_tools() {
        use iocraft::prelude::ElementExt as _;

        // The file components read AppState, so the render harness needs the
        // provider around the theme context.
        let render = |element: std::sync::Arc<std::sync::Mutex<Option<AnyElement<'static>>>>| {
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            iocraft::prelude::element! {
                iocraft::prelude::ContextProvider(
                    value: iocraft::prelude::Context::owned(*crate::utils::theme::current()),
                ) {
                    crate::state::app_state::AppStateProvider(
                        prebuilt_store: Some(store),
                        children: crate::state::app_state::ProviderChildren::new(move || {
                            element.lock().unwrap().take().expect("element rendered once")
                        }),
                    )
                }
            }
            .render(Some(80))
            .to_string()
        };
        let once = |element: AnyElement<'static>| {
            std::sync::Arc::new(std::sync::Mutex::new(Some(element)))
        };

        // Non-file tools never take this path.
        for tool in ["Read", "Bash", "Grep", "Agent", "UnknownTool"] {
            assert!(
                official_file_result_element(
                    tool,
                    ToolResultStatus::Success,
                    None,
                    None,
                    false,
                    None
                )
                .is_none(),
                "{tool} must fall through to the line pipeline"
            );
        }

        // A file tool with no raw renders nothing rather than falling through
        // — CC's success leaf returns null when `toolUseResult` is missing
        // (`UserToolSuccessMessage.tsx:72`).
        let empty = official_file_result_element(
            "Edit",
            ToolResultStatus::Success,
            None,
            None,
            false,
            None,
        )
        .expect("Edit claims the element path");
        assert_eq!(render(once(empty)).trim(), "");

        // Write update renders the shared Edit component from the raw
        // (`FileWriteTool/UI.tsx:320-333`); the create arm deliberately falls
        // through to the line pipeline (FileWriteToolCreatedMessage rows).
        let update_raw = serde_json::json!({
            "type": "update",
            "filePath": "/tmp/existing.txt",
            "content": "new line",
            "structuredPatch": [{
                "oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                "lines": ["-old line", "+new line"]
            }],
            "originalFile": "old line"
        });
        let updated = official_file_result_element(
            "Write",
            ToolResultStatus::Success,
            Some(&update_raw),
            None,
            false,
            None,
        )
        .expect("Write update claims the element path");
        let canvas = render(once(updated));
        assert!(
            canvas.contains("Added 1 line, removed 1 line"),
            "canvas=\n{canvas}"
        );
        assert!(canvas.contains("+new line"), "canvas=\n{canvas}");

        let create_raw = serde_json::json!({
            "type": "create",
            "filePath": "/tmp/new.txt",
            "content": "one\ntwo",
            "structuredPatch": [],
            "originalFile": null
        });
        assert!(
            official_file_result_element(
                "Write",
                ToolResultStatus::Success,
                Some(&create_raw),
                None,
                false,
                None,
            )
            .is_none(),
            "Write create renders through the line pipeline"
        );

        // A rejected Write has no raw at all — the preview comes from the
        // INPUT, so the element must still render content.
        let write_input = serde_json::json!({
            "file_path": "/tmp/new.txt",
            "content": "one\ntwo"
        });
        let rejected = official_file_result_element(
            "Write",
            ToolResultStatus::Rejected,
            None,
            Some(&write_input),
            false,
            None,
        )
        .expect("rejected Write claims the element path");
        let canvas = render(once(rejected));
        assert!(!canvas.trim().is_empty(), "rejected preview must render");

        // Aliases resolve like their canonical names.
        for alias in ["MultiEdit", "FileEdit", "FileWrite"] {
            assert!(
                official_file_result_element(
                    alias,
                    ToolResultStatus::Success,
                    None,
                    None,
                    false,
                    None
                )
                .is_some(),
                "{alias} must resolve like its canonical name"
            );
        }
    }

    #[test]
    fn recovered_write_tool_use_result_retains_original_file_and_git_diff() {
        // No Write display shape — the cold projection is Generic and the
        // raw parses back through the tool's own schema, keeping
        // originalFile and gitDiff intact.
        let raw = serde_json::json!({
            "type": "update",
            "filePath": "src/a.rs",
            "content": "new",
            "originalFile": "old",
            "structuredPatch": [],
            "gitDiff": {
                "filename": "src/a.rs",
                "status": "modified",
                "additions": 1,
                "deletions": 1,
                "changes": 2,
                "patch": "@@ -1 +1 @@\n-old\n+new\n",
                "repository": "owner/repository"
            }
        });
        let output =
            crate::tools::file_write_tool::ui::parse_output(&raw).expect("write raw parses");
        assert_eq!(output.original_file.as_deref(), Some("old"));
        let diff = output.git_diff.expect("gitDiff survives");
        assert_eq!(
            diff.status,
            crate::utils::git_diff::ToolUseDiffStatus::Modified
        );
        assert_eq!((diff.additions, diff.deletions), (1, 1));
        assert_eq!(diff.repository.as_deref(), Some("owner/repository"));
    }

    /// No Glob display shape — the raw rides the row and the by-tool-name
    /// dispatch renders from it; `transcript_tool_result_should_emit_ui` owns
    /// the emit gate for malformed success output.
    #[test]
    fn recovered_glob_raw_outputs_render_by_tool_name() {
        let success_raw = serde_json::json!({
            "durationMs": 23,
            "numFiles": 2,
            "filenames": ["src/lib.rs", "/outside/src/main.rs"],
            "truncated": true
        });
        let lines = render_tool_result_lines_for_result(
            "Glob",
            ToolResultStatus::Success,
            "",
            Some(&success_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(lines[0].text.starts_with("Found 2 files"));
        assert!(lines[0].text.contains("ctrl+o to expand"));

        // Malformed success output renders nothing — the by-tool-name
        // renderer rejects the raw (`UserToolSuccessMessage.tsx:81`).
        assert!(
            render_tool_result_lines_for_result(
                "Glob",
                ToolResultStatus::Success,
                "src/lib.rs",
                Some(&serde_json::json!({
                    "durationMs": 1,
                    "numFiles": 1,
                    "filenames": ["src/lib.rs"]
                })),
                None,
                &[],
                ToolRenderOptions::default(),
            )
            .is_empty()
        );
    }

    #[test]
    fn user_tool_result_dispatches_bash_success_error_and_missing_raw() {
        // Bash renders from the raw `toolUseResult` (CC wire shape),
        // dispatched by tool name; the row carries no display shape.
        let success_raw = serde_json::json!({
            "stdout": "ok",
            "stderr": "",
            "interrupted": false,
            "noOutputExpected": false
        });
        let success = render_tool_result_lines_for_result(
            "Bash",
            ToolResultStatus::Success,
            "",
            Some(&success_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(success[0].text, "ok");
        assert!(!success.iter().any(|line| line.text.starts_with("$ ")));

        // Error must use FallbackToolUseErrorMessage semantics (mapped
        // content, 10 raw lines) — never OutputLine's 3-line fold
        // (BashTool/UI.tsx:200-213).
        let error = render_tool_result_lines(
            "Bash",
            ToolResultStatus::Error,
            "line0\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11",
        );
        assert!(error.iter().any(|line| line.tone == ToolRenderTone::Error));
        assert!(error[0].text.contains("line0"));
        assert!(error[0].text.contains("line9"));
        assert!(!error[0].text.contains("line11"));
        assert!(
            error
                .iter()
                .any(|line| line.text.contains("+2 lines") && line.text.contains("to see all"))
        );

        // A Success row with no raw renders nothing
        // (`UserToolSuccessMessage.tsx:72`), same rule as Read.
        assert!(render_tool_result_lines("Bash", ToolResultStatus::Success, "ignored",).is_empty());
    }

    #[test]
    fn user_tool_result_dispatches_bash_official_image_and_background_rows() {
        let render = |raw: serde_json::Value| {
            render_tool_result_lines_for_result(
                "Bash",
                ToolResultStatus::Success,
                "",
                Some(&raw),
                None,
                &[],
                ToolRenderOptions::default(),
            )
        };

        let image = render(serde_json::json!({
            "stdout": "data:image/png;base64,abc",
            "stderr": "",
            "interrupted": false,
            "isImage": true,
            "noOutputExpected": false
        }));
        assert_eq!(image[0].text, "[Image data detected and sent to Claude]");
        assert_eq!(image[0].tone, ToolRenderTone::Inactive);

        let background = render(serde_json::json!({
            "stdout": "",
            "stderr": "",
            "interrupted": false,
            "noOutputExpected": false,
            "backgroundTaskId": "bash_1"
        }));
        assert_eq!(
            background[0].text,
            "Running in the background (↓ to manage)"
        );
        assert_eq!(background[0].tone, ToolRenderTone::Inactive);
    }

    #[test]
    fn user_tool_result_dispatches_powershell_official_empty_rows() {
        let render = |raw: serde_json::Value| {
            render_tool_result_lines_for_result(
                "PowerShell",
                ToolResultStatus::Success,
                "",
                Some(&raw),
                None,
                &[],
                ToolRenderOptions::default(),
            )
        };

        let image = render(serde_json::json!({
            "stdout": "",
            "stderr": "",
            "interrupted": false,
            "isImage": true
        }));
        assert_eq!(image[0].text, "[Image data detected and sent to Claude]");
        assert_eq!(image[0].tone, ToolRenderTone::Inactive);

        let background = render(serde_json::json!({
            "stdout": "",
            "stderr": "",
            "interrupted": false,
            "backgroundTaskId": "ps-1"
        }));
        assert_eq!(
            background[0].text,
            "Running in the background (↓ to manage)"
        );
        assert_eq!(background[0].tone, ToolRenderTone::Inactive);

        // PowerShell's empty ladder: interrupted before
        // returnCodeInterpretation (PowerShellTool/UI.tsx:143-158).
        let interrupted = render(serde_json::json!({
            "stdout": "",
            "stderr": "",
            "interrupted": true,
            "returnCodeInterpretation": "should not show"
        }));
        assert_eq!(interrupted[0].text, "Interrupted");
    }

    #[test]
    fn user_tool_result_dispatches_read_official_non_text_rows() {
        // Read renders from the raw `toolUseResult` (CC's wire shapes),
        // dispatched by tool name; the display carries nothing Read-specific.
        let render = |raw: serde_json::Value| {
            render_tool_result_lines_for_result(
                "Read",
                ToolResultStatus::Success,
                "",
                Some(&raw),
                None,
                &[],
                ToolRenderOptions::default(),
            )
        };

        let pdf = render(serde_json::json!({
            "type": "pdf",
            "file": {"filePath": "paper.pdf", "base64": "AA==", "originalSize": 4096}
        }));
        assert_eq!(pdf[0].text, "Read PDF (4KB)");

        let notebook = render(serde_json::json!({
            "type": "notebook",
            "file": {"filePath": "demo.ipynb", "cells": [{}, {}, {}]}
        }));
        assert_eq!(notebook[0].text, "Read 3 cells");

        let empty_notebook = render(serde_json::json!({
            "type": "notebook",
            "file": {"filePath": "empty.ipynb", "cells": []}
        }));
        assert_eq!(empty_notebook[0].text, "No cells found in notebook");
        assert_eq!(empty_notebook[0].tone, ToolRenderTone::Error);

        let parts = render(serde_json::json!({
            "type": "parts",
            "file": {"filePath": "pages.pdf", "originalSize": 1024, "count": 1, "outputDir": "/tmp/pages"}
        }));
        assert_eq!(parts[0].text, "Read 1 page (1KB)");

        let unchanged = render(serde_json::json!({
            "type": "file_unchanged",
            "file": {"filePath": "cached.txt"}
        }));
        assert_eq!(unchanged[0].text, "Unchanged since last read");
        assert_eq!(unchanged[0].tone, ToolRenderTone::Inactive);
    }

    #[test]
    fn user_tool_result_dispatches_read_edit_write_search_summaries() {
        // CC FileWriteTool/UI.tsx:59 uses relative(getCwd(), filePath).
        // These raw fixtures contain relative paths, so the session and process
        // cwd must agree for the source to display the bare filename.
        struct RestoreCwd(std::path::PathBuf);
        impl Drop for RestoreCwd {
            fn drop(&mut self) {
                crate::bootstrap::state::set_original_cwd(self.0.clone());
            }
        }
        let _cwd = RestoreCwd(crate::bootstrap::state::get_original_cwd());
        crate::bootstrap::state::set_original_cwd(std::env::current_dir().unwrap());
        // Read renders from raw `toolUseResult`, dispatched by name.
        let read = |status: ToolResultStatus, content: &str, raw: Option<serde_json::Value>| {
            render_tool_result_lines_for_result(
                "Read",
                status,
                content,
                raw.as_ref(),
                None,
                &[],
                ToolRenderOptions::default(),
            )
        };
        assert_eq!(
            read(
                ToolResultStatus::Success,
                "",
                Some(serde_json::json!({
                    "type": "text",
                    "file": {"filePath": "src/main.rs", "content": "", "numLines": 42, "startLine": 1, "totalLines": 42}
                })),
            )[0]
            .text,
            "Read 42 lines"
        );
        assert_eq!(
            read(
                ToolResultStatus::Success,
                "",
                Some(serde_json::json!({
                    "type": "image",
                    "file": {"base64": "AA==", "type": "image/png", "originalSize": 2048}
                })),
            )[0]
            .text,
            "Read image (2KB)"
        );
        let read_error = read(ToolResultStatus::Error, "Error: File not found", None);
        // CC keeps the `Error: ` prefix: `FallbackToolUseErrorMessage.tsx:41-46`
        // passes a prefixed message through verbatim (and ADDS the prefix when
        // missing). The old ReadError arm stripped it — a deviation this
        // dispatch-by-name path retires; the error leaf component
        // (`user_tool_error_message.rs:69-86`) already renders it CC's way.
        assert_eq!(read_error[0].text, "Error: File not found");
        assert_eq!(read_error[0].tone, ToolRenderTone::Error);

        // Edit renders its transcript rows from the raw `toolUseResult`.
        let edit_raw = serde_json::json!({
            "filePath": "src/lib.rs",
            "oldString": "old",
            "newString": "new",
            "originalFile": "old",
            "structuredPatch": [{
                "oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                "lines": ["-old", "+new"]
            }],
            "userModified": false,
            "replaceAll": false
        });
        let edit_lines = render_tool_result_lines_for_result(
            "Edit",
            ToolResultStatus::Success,
            "",
            Some(&edit_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(edit_lines[0].text, "Added 1 line, removed 1 line");
        assert!(edit_lines[1].text.contains("1 -old"));
        assert_eq!(edit_lines[1].tone, ToolRenderTone::Normal);
        assert_eq!(
            edit_lines[1].background,
            Some(ToolRenderBackground::DiffRemoved)
        );
        assert!(edit_lines[2].text.contains("1 +new"));
        assert_eq!(edit_lines[2].tone, ToolRenderTone::Normal);
        assert_eq!(
            edit_lines[2].background,
            Some(ToolRenderBackground::DiffAdded)
        );

        let write_create_raw = serde_json::json!({
            "type": "create",
            "filePath": "README.md",
            "content": "one\ntwo\nthree",
            "structuredPatch": [],
            "originalFile": null
        });
        let write_lines = render_tool_result_lines_for_result(
            "Write",
            ToolResultStatus::Success,
            "",
            Some(&write_create_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(write_lines[0].text, "Wrote 3 lines to README.md");
        assert_eq!(write_lines[1].text, " 1 one");
        assert_eq!(write_lines[2].text, " 2 two");
        assert_eq!(write_lines[3].text, " 3 three");
        assert!(!write_lines[1].segments.is_empty());

        let write_update_raw = serde_json::json!({
            "type": "update",
            "filePath": "README.md",
            "content": "new",
            "structuredPatch": [{
                "oldStart": 1, "oldLines": 2, "newStart": 1, "newLines": 2,
                "lines": [" context", "-old", "+new"]
            }],
            "originalFile": "old"
        });
        let write_update_lines = render_tool_result_lines_for_result(
            "Write",
            ToolResultStatus::Success,
            "",
            Some(&write_update_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(write_update_lines[0].text, "Added 1 line, removed 1 line");
        assert_eq!(write_update_lines[1].tone, ToolRenderTone::Normal);
        assert!(write_update_lines[1].dim);
        assert_eq!(write_update_lines[2].tone, ToolRenderTone::Normal);
        assert_eq!(
            write_update_lines[2].background,
            Some(ToolRenderBackground::DiffRemoved)
        );
        assert_eq!(write_update_lines[3].tone, ToolRenderTone::Normal);
        assert_eq!(
            write_update_lines[3].background,
            Some(ToolRenderBackground::DiffAdded)
        );

        // Grep renders from the raw `toolUseResult` on the row.
        let count_raw = serde_json::json!({
            "mode": "count",
            "numFiles": 2,
            "filenames": ["a.rs", "b.rs"],
            "content": "a.rs:1\nb.rs:1",
            "numMatches": 2
        });
        let count_lines = render_tool_result_lines_for_result(
            "Grep",
            ToolResultStatus::Success,
            "",
            Some(&count_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(
            count_lines[0]
                .text
                .starts_with("Found 2 matches across 2 files")
        );
        assert!(count_lines[0].text.contains("ctrl+o to expand"));
        assert!(!count_lines.iter().any(|line| line.text.contains("a.rs")));
        // Untagged error copy flows through FallbackToolUseErrorMessage.
        let search_error =
            render_tool_result_lines("Grep", ToolResultStatus::Error, "Error: File not found");
        assert_eq!(search_error[0].text, "Error: File not found");
        assert_eq!(search_error[0].tone, ToolRenderTone::Error);

        let glob_validation_error = render_tool_result_lines(
            "Glob",
            ToolResultStatus::Error,
            "<tool_use_error>Directory does not exist: missing. Note: your current working directory is /repo.</tool_use_error>",
        );
        assert_eq!(glob_validation_error[0].text, "File not found");
        assert_eq!(glob_validation_error[0].tone, ToolRenderTone::Error);
        let glob_runtime_error = render_tool_result_lines(
            "Glob",
            ToolResultStatus::Error,
            "<tool_use_error>ripgrep failed</tool_use_error>",
        );
        assert_eq!(glob_runtime_error[0].text, "Error searching files");

        // Glob renders from the raw on the row; LS still rides the shared
        // Search display until its own migration.
        let glob_lines = render_tool_result_lines_for_result(
            "Glob",
            ToolResultStatus::Success,
            "",
            Some(&serde_json::json!({
                "durationMs": 3,
                "numFiles": 1,
                "filenames": ["src/main.rs"],
                "truncated": false
            })),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(glob_lines[0].text.starts_with("Found 1 file"));
        assert!(glob_lines[0].text.contains("ctrl+o to expand"));
        assert!(
            !glob_lines
                .iter()
                .any(|line| line.text.contains("src/main.rs"))
        );

        // CC 2.1.88 has no LS tool: an old transcript's LS row resolves no
        // tool and renders through the generic fallback.
        let ls_lines = render_tool_result_lines("LS", ToolResultStatus::Success, "src/main.rs");
        assert_eq!(ls_lines[0].text, "src/main.rs");
    }

    /// The by-tool-name dispatch hands Grep the raw `toolUseResult` and its
    /// own renderer parses it (`UserToolSuccessMessage.tsx:80-96` →
    /// `GrepTool/UI.tsx:126-172`); the display carries no Grep shape.
    #[test]
    fn grep_user_tool_result_dispatch_follows_official_output_modes() {
        let content_raw = serde_json::json!({
            "mode": "content",
            "numFiles": 2,
            "filenames": [],
            "content": "a.rs:1:foo\nb.rs:2:foo",
            "numLines": 3
        });
        let content_lines = render_tool_result_lines_for_result(
            "Grep",
            ToolResultStatus::Success,
            "",
            Some(&content_raw),
            None,
            &[],
            ToolRenderOptions {
                verbose: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(content_lines[0].text, "Found 3 lines");
        assert_eq!(content_lines[1].text, "a.rs:1:foo\nb.rs:2:foo");

        let count_raw = serde_json::json!({
            "mode": "count",
            "numFiles": 1,
            "filenames": ["a.rs"],
            "content": "a.rs:1",
            "numMatches": 1
        });
        let count_lines = render_tool_result_lines_for_result(
            "Grep",
            ToolResultStatus::Success,
            "",
            Some(&count_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(
            count_lines[0]
                .text
                .starts_with("Found 1 matche across 1 file")
        );
        assert!(count_lines[0].text.contains("ctrl+o to expand"));
        assert!(!count_lines.iter().any(|line| line.text.contains("a.rs")));

        // A Grep row with no raw renders nothing, exactly as CC's success
        // leaf returns null before reaching the tool.
        assert!(
            render_tool_result_lines_for_result(
                "Grep",
                ToolResultStatus::Success,
                "",
                None,
                None,
                &[],
                ToolRenderOptions::default(),
            )
            .is_empty()
        );
    }

    #[test]
    fn user_tool_result_rejected_edit_without_raw_renders_nothing_in_line_pipeline() {
        // The rejected Edit preview is an element computed from the tool
        // INPUT (`official_file_result_element`); the line pipeline renders
        // nothing for a raw-less rejected row.
        assert!(render_tool_result_lines("Edit", ToolResultStatus::Rejected, "",).is_empty());
    }

    #[test]
    fn user_tool_result_renders_lsp_summary_like_official_tool_ui() {
        // The raw `toolUseResult` on the row drives the renderer.
        let collapsed_raw = serde_json::json!({
            "operation": "findReferences",
            "result": "src/main.rs:7:12\nsrc/lib.rs:3:4",
            "filePath": "src/main.rs",
            "resultCount": 2,
            "fileCount": 2,
        });
        let collapsed = render_tool_result_lines_for_result(
            "LSP",
            ToolResultStatus::Success,
            "Found 2 references",
            Some(&collapsed_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            collapsed[0].text,
            "Found 2 references across 2 files (ctrl+o to expand)"
        );
        assert_eq!(collapsed[0].tone, ToolRenderTone::Normal);
        // "{resultCount} " and "{fileCount} " render bold; the hint is dim.
        assert!(collapsed[0].segments[1].bold);
        assert!(collapsed[0].segments.last().unwrap().dim);

        let verbose_raw = serde_json::json!({
            "operation": "goToDefinition",
            "result": "src/main.rs:7:12",
            "filePath": "src/main.rs",
            "resultCount": 1,
            "fileCount": 1,
        });
        let verbose = render_tool_result_lines_for_result(
            "LSP",
            ToolResultStatus::Success,
            "Found 1 definition",
            Some(&verbose_raw),
            None,
            &[],
            ToolRenderOptions {
                verbose: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(verbose[0].text, "Found 1 definition");
        assert_eq!(verbose[1].text, "src/main.rs:7:12");

        let hover_raw = serde_json::json!({
            "operation": "hover",
            "result": "hover text",
            "filePath": "src/main.rs",
            "resultCount": 1,
            "fileCount": 1,
        });
        let hover = render_tool_result_lines_for_result(
            "LSP",
            ToolResultStatus::Success,
            "hover text",
            Some(&hover_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(hover[0].text, "Hover info available (ctrl+o to expand)");

        // Counts absent → the bare `{output.result}` fallback branch
        // (UI.tsx:196-202); a non-enum operation fails safeParse → nothing,
        // and the caller's empty-lines fallback takes over.
        let error_raw = serde_json::json!({
            "operation": "findReferences",
            "result": "Error performing findReferences: not connected",
            "filePath": "src/main.rs",
        });
        let fallback = render_tool_result_lines_for_result(
            "LSP",
            ToolResultStatus::Success,
            "",
            Some(&error_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            fallback[0].text,
            "Error performing findReferences: not connected"
        );
    }

    /// The by-tool-name dispatch hands both worktree tools the raw
    /// `toolUseResult` and each renderer parses it with its own schema.
    #[test]
    fn user_tool_result_renders_worktree_results_like_official_tool_ui() {
        let enter_lines = render_tool_result_lines_for_result(
            "EnterWorktree",
            ToolResultStatus::Success,
            "Created worktree",
            Some(&serde_json::json!({
                "worktreePath": "/tmp/project-feature",
                "worktreeBranch": "feature",
                "message": "created"
            })),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            enter_lines[0].text,
            "Switched to worktree on branch feature"
        );
        assert_eq!(enter_lines[0].tone, ToolRenderTone::Normal);
        assert_eq!(enter_lines[1].text, "/tmp/project-feature");
        assert_eq!(enter_lines[1].tone, ToolRenderTone::Inactive);

        let exit_lines = render_tool_result_lines_for_result(
            "ExitWorktree",
            ToolResultStatus::Success,
            "Kept worktree",
            Some(&serde_json::json!({
                "action": "keep",
                "originalCwd": "/tmp/project",
                "worktreePath": "/tmp/project-feature",
                "worktreeBranch": "feature",
                "message": "kept"
            })),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(exit_lines[0].text, "Kept worktree (branch feature)");
        assert_eq!(exit_lines[1].text, "Returned to /tmp/project");
        assert_eq!(exit_lines[1].tone, ToolRenderTone::Inactive);
    }

    #[test]
    fn user_tool_result_renders_ask_user_question_like_official_tool_ui() {
        // The raw `toolUseResult` on the row drives the renderer.
        let raw = serde_json::json!({
            "questions": [],
            "answers": {"Proceed?": "Yes"},
        });
        let answered = render_tool_result_lines_for_result(
            "AskUserQuestion",
            ToolResultStatus::Success,
            "User answered Claude's questions:\n· Proceed? → Yes",
            Some(&raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(
            answered[0]
                .text
                .ends_with(" User answered Claude's questions:")
        );
        assert_eq!(answered[1].text, "· Proceed? → Yes");
        assert_eq!(answered[1].tone, ToolRenderTone::Inactive);

        // The rejected leaf renders from the tool name via
        // `UserToolRejectMessage` (`UserToolRejectMessage.tsx:46`).
        let rejected = crate::components::messages::user_tool_result_message::user_tool_reject_message::render_tool_use_rejected_lines(
            "AskUserQuestion",
            None,
        )
        .expect("AskUserQuestion has a rejected renderer");
        assert!(
            rejected[0]
                .text
                .ends_with(" User declined to answer questions")
        );
        assert_eq!(rejected[0].tone, ToolRenderTone::Normal);
    }

    #[test]
    fn user_tool_result_renders_enter_exit_plan_mode_results_like_official_tool_ui() {
        // The raw `toolUseResult` on the row drives the renderer.
        let raw = serde_json::json!({"message": "Entered plan mode."});
        let entered = render_tool_result_lines_for_result(
            "EnterPlanMode",
            ToolResultStatus::Success,
            "Entered plan mode",
            Some(&raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            entered[0].text,
            format!(
                "{} Entered plan mode",
                crate::constants::figures::BLACK_CIRCLE
            )
        );
        assert_eq!(entered[0].tone, ToolRenderTone::Normal);
        assert_eq!(
            entered[1].text,
            "Claude is now exploring and designing an implementation approach."
        );
        assert_eq!(entered[1].tone, ToolRenderTone::Inactive);

        // The rejected leaf renders from the tool name via
        // `UserToolRejectMessage`.
        let enter_rejected = crate::components::messages::user_tool_result_message::user_tool_reject_message::render_tool_use_rejected_lines(
            "EnterPlanMode",
            None,
        )
        .expect("EnterPlanMode has a rejected renderer");
        assert_eq!(
            enter_rejected[0].text,
            format!(
                "{} User declined to enter plan mode",
                crate::constants::figures::BLACK_CIRCLE
            )
        );

        // The raw `toolUseResult` on the row drives the renderer.
        let exit_raw = |plan: serde_json::Value, file_path: Option<&str>, awaiting: bool| {
            let mut raw = serde_json::json!({"plan": plan, "isAgent": false});
            if let Some(file_path) = file_path {
                raw["filePath"] = serde_json::json!(file_path);
            }
            if awaiting {
                raw["awaitingLeaderApproval"] = serde_json::json!(true);
            }
            raw
        };
        let raw = exit_raw(serde_json::json!(""), None, false);
        let empty_exit = render_tool_result_lines_for_result(
            "ExitPlanMode",
            ToolResultStatus::Success,
            "Exited plan mode",
            Some(&raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            empty_exit[0].text,
            format!(
                "{} Exited plan mode",
                crate::constants::figures::BLACK_CIRCLE
            )
        );

        let raw = exit_raw(
            serde_json::json!("## Plan\nShip it"),
            Some("/tmp/project/.nyan/plans/plan.md"),
            false,
        );
        let approved = render_tool_result_lines_for_result(
            "ExitPlanMode",
            ToolResultStatus::Success,
            "User approved Claude's plan",
            Some(&raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            approved[0].text,
            format!(
                "{} User approved Claude's plan",
                crate::constants::figures::BLACK_CIRCLE
            )
        );
        assert_eq!(
            approved[1].text,
            "Plan saved to: /tmp/project/.nyan/plans/plan.md · /plan to edit"
        );
        assert_eq!(approved[1].tone, ToolRenderTone::Inactive);
        assert_eq!(approved[2].text, "## Plan");

        let raw = exit_raw(
            serde_json::json!("## Plan"),
            Some("/tmp/project/.nyan/plans/plan.md"),
            true,
        );
        let awaiting = render_tool_result_lines_for_result(
            "ExitPlanMode",
            ToolResultStatus::Success,
            "Plan submitted",
            Some(&raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            awaiting[0].text,
            format!(
                "{} Plan submitted for team lead approval",
                crate::constants::figures::BLACK_CIRCLE
            )
        );
        assert_eq!(
            awaiting[2].text,
            "Waiting for team lead to review and approve..."
        );

        // A nullable-required plan: `plan: null` parses, a missing plan key
        // fails safeParse and renders nothing (empty falls to generic).
        let null_plan = exit_raw(serde_json::Value::Null, None, false);
        let exited = render_tool_result_lines_for_result(
            "ExitPlanMode",
            ToolResultStatus::Success,
            "Exited plan mode",
            Some(&null_plan),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(
            exited[0].text,
            format!(
                "{} Exited plan mode",
                crate::constants::figures::BLACK_CIRCLE
            )
        );

        // Maps to: CC `UserToolErrorMessage.tsx:52-59` — the central error
        // leaf routes PLAN_REJECTION_PREFIX content to RejectedPlanMessage.
        let rejected = render_tool_result_lines_for_result(
            "ExitPlanMode",
            ToolResultStatus::Error,
            &format!(
                "{}## Plan\nRevise",
                crate::utils::messages::PLAN_REJECTION_PREFIX
            ),
            None,
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(rejected[0].text, "User rejected Claude's plan:");
        assert_eq!(rejected[0].tone, ToolRenderTone::Inactive);
        assert_eq!(rejected[1].text, "## Plan");
    }

    #[test]
    fn user_tool_result_renders_task_stop_result_like_official_tool_ui() {
        // The raw `toolUseResult` on the row drives the renderer.
        let raw = |command: &str| {
            serde_json::json!({
                "message": format!("Successfully stopped task: task_1 ({command})"),
                "task_id": "task_1",
                "task_type": "local_bash",
                "command": command,
            })
        };
        let value = raw("cargo test");
        let lines = render_tool_result_lines_for_result(
            "TaskStop",
            ToolResultStatus::Success,
            "{\"task_id\":\"task_1\"}",
            Some(&value),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "cargo test · stopped");

        let value = raw("one\ntwo\nthree");
        let truncated = render_tool_result_lines_for_result(
            "TaskStop",
            ToolResultStatus::Success,
            "{\"task_id\":\"task_1\"}",
            Some(&value),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(truncated[0].text, "one\ntwo… · stopped");

        // KillShell is TaskStopTool's deprecated alias.
        let value = raw("cargo test");
        let alias = render_tool_result_lines_for_result(
            "KillShell",
            ToolResultStatus::Success,
            "{\"task_id\":\"task_1\"}",
            Some(&value),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(alias[0].text, "cargo test · stopped");
    }

    #[test]
    fn user_tool_result_renders_send_message_result_like_official_tool_ui() {
        // The raw `toolUseResult` on the row drives the renderer.
        let raw = serde_json::json!({"success": true, "message": "Response sent"});
        let lines = render_tool_result_lines_for_result(
            "SendMessage",
            ToolResultStatus::Success,
            "Response sent",
            Some(&raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "Response sent");
        assert_eq!(lines[0].tone, ToolRenderTone::Inactive);
    }

    #[test]
    fn user_tool_result_renders_skill_result_like_official_tool_ui() {
        // The raw `toolUseResult` on the row drives the renderer.
        let inline_raw = serde_json::json!({
            "success": true,
            "commandName": "review-pr",
            "allowedTools": ["Read"],
            "model": "opus",
        });
        let inline = render_tool_result_lines_for_result(
            "Skill",
            ToolResultStatus::Success,
            "Launching skill: review-pr",
            Some(&inline_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(inline[0].text, "Successfully loaded skill · 1 tool allowed");
        assert_eq!(inline[0].tone, ToolRenderTone::Normal);

        let forked_raw = serde_json::json!({
            "success": true,
            "commandName": "legacy",
            "status": "forked",
            "agentId": "agent-1",
            "result": "done",
        });
        let forked = render_tool_result_lines_for_result(
            "Skill",
            ToolResultStatus::Success,
            "Skill completed",
            Some(&forked_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(forked[0].text, "Done");
    }

    /// The by-tool-name dispatch hands NotebookEdit the raw
    /// `toolUseResult` and its own renderer parses it
    /// (`UserToolSuccessMessage.tsx:80-96` → `NotebookEditTool/UI.tsx:99-127`);
    /// the rejected leaf keeps its input-driven display variant.
    #[test]
    fn user_tool_result_dispatches_notebook_edit_official_rows() {
        let success_raw = serde_json::json!({
            "new_source": "print('hello')",
            "cell_id": "cell-1",
            "cell_type": "code",
            "language": "python",
            "edit_mode": "replace",
            "error": "",
            "notebook_path": "/repo/notebooks/demo.ipynb",
            "original_file": "old",
            "updated_file": "new"
        });
        let lines = render_tool_result_lines_for_result(
            "NotebookEdit",
            ToolResultStatus::Success,
            "Updated cell cell-1 with print('hello')",
            Some(&success_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "Updated cell cell-1:");
        assert!(lines.iter().any(|line| line.text.contains("print")));

        let validation_error = render_tool_result_lines(
            "NotebookEdit",
            ToolResultStatus::Error,
            "<tool_use_error>Notebook file does not exist.</tool_use_error>",
        );
        assert_eq!(validation_error[0].text, "Error editing notebook");
        // CC's renderToolUseErrorMessage compacts only tagged copy
        // (UI.tsx:84-89); untagged text flows to the fallback verbatim.
        let untagged_error = render_tool_result_lines(
            "NotebookEdit",
            ToolResultStatus::Error,
            "Error: Notebook file does not exist.",
        );
        assert_eq!(
            untagged_error[0].text,
            "Error: Notebook file does not exist."
        );

        // The rejected leaf renders from the tool INPUT via
        // `UserToolRejectMessage`, which mounts the dedicated
        // `NotebookEditToolUseRejectedMessage` component — there is no
        // line-pipeline copy of that copy (the former one was dead code,
        // removed with the #75 element-pipeline migration).
    }

    #[test]
    fn notebook_validation_error_component_uses_compact_nonverbose_copy() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "NotebookEdit".to_string(),
                    is_error: true,
                    content: "<tool_use_error>Cell with ID missing.</tool_use_error>".to_string(),                    tool_use_result: Some(serde_json::Value::String(
                        "Error: Cell with ID missing.".to_string(),
                    )),
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(100))
        .to_string();
        assert!(text.contains("Error editing notebook"), "canvas=\n{text}");
        assert!(!text.contains("Cell with ID"), "canvas=\n{text}");
    }

    #[test]
    fn user_tool_result_renders_structured_output_like_official_tool_ui() {
        // The raw `toolUseResult` is CC's bare Output string.
        let raw = serde_json::json!("Structured output provided successfully");
        let lines = render_tool_result_lines_for_result(
            "StructuredOutput",
            ToolResultStatus::Success,
            "Structured output provided successfully",
            Some(&raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(lines[0].text, "Structured output provided successfully");
        assert_eq!(lines[0].tone, ToolRenderTone::Normal);

        // The error leaf is the bare constant `renderToolUseErrorMessage`
        // string, rendered uncolored (CC returns a naked string).
        let errored = render_tool_result_lines_for_result(
            "StructuredOutput",
            ToolResultStatus::Error,
            "<tool_use_error>Output does not match required schema</tool_use_error>",
            None,
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(errored[0].text, "Structured output error");
        assert_eq!(errored[0].tone, ToolRenderTone::Normal);
    }

    #[test]
    fn user_tool_result_renders_task_output_result_like_official_tool_ui() {
        // The raw `toolUseResult` on the row drives the renderer.
        let success_raw = serde_json::json!({
            "retrieval_status": "success",
            "task": {
                "task_id": "task-7",
                "task_type": "local_agent",
                "status": "completed",
                "description": "Review auth",
                "output": "Done",
                "result": "Done",
                "prompt": "Inspect auth",
            },
        });
        let local_agent = render_tool_result_lines_for_result(
            "TaskOutput",
            ToolResultStatus::Success,
            "",
            Some(&success_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(local_agent[0].text, "Read output (ctrl+o to expand)");
        assert_eq!(local_agent[0].tone, ToolRenderTone::Inactive);

        let running_raw = serde_json::json!({
            "retrieval_status": "not_ready",
            "task": {
                "task_id": "task-8",
                "task_type": "local_agent",
                "status": "running",
                "description": "Review auth",
                "output": "",
            },
        });
        // BashOutput is a TaskOutputTool alias (TaskOutputTool.tsx:176).
        let running = render_tool_result_lines_for_result(
            "BashOutput",
            ToolResultStatus::Success,
            "",
            Some(&running_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(running[0].text, "Task is still running…");
    }

    /// The by-tool-name dispatch hands the cron tools the raw
    /// `toolUseResult` and each renderer parses it with its own schema.
    #[test]
    fn user_tool_result_renders_schedule_cron_results_like_official_tool_ui() {
        let create_lines = render_tool_result_lines_for_result(
            "CronCreate",
            ToolResultStatus::Success,
            "Scheduled recurring job cron_1",
            Some(&serde_json::json!({
                "id": "cron_1",
                "humanSchedule": "hourly at :07",
                "recurring": true
            })),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(create_lines[0].text, "Scheduled cron_1 (hourly at :07)");

        let delete_lines = render_tool_result_lines_for_result(
            "CronDelete",
            ToolResultStatus::Success,
            "Cancelled job cron_1.",
            Some(&serde_json::json!({"id": "cron_1"})),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(delete_lines[0].text, "Cancelled cron_1");

        let empty_list_lines = render_tool_result_lines_for_result(
            "CronList",
            ToolResultStatus::Success,
            "No scheduled jobs.",
            Some(&serde_json::json!({"jobs": []})),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(empty_list_lines[0].text, "No scheduled jobs");
        assert_eq!(empty_list_lines[0].tone, ToolRenderTone::Inactive);

        let list_lines = render_tool_result_lines_for_result(
            "CronList",
            ToolResultStatus::Success,
            "cron_2 — daily",
            Some(&serde_json::json!({
                "jobs": [{
                    "id": "cron_2",
                    "cron": "0 9 * * *",
                    "humanSchedule": "daily at 09:00",
                    "prompt": "check"
                }]
            })),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(list_lines[0].text, "cron_2 daily at 09:00");

        // Missing or schema-rejected raw renders nothing.
        assert!(
            render_tool_result_lines_for_result(
                "CronCreate",
                ToolResultStatus::Success,
                "",
                None,
                None,
                &[],
                ToolRenderOptions::default(),
            )
            .is_empty()
        );
    }

    /// The by-tool-name dispatch hands WebFetch/WebSearch the raw
    /// `toolUseResult` and each tool's own renderer parses it.
    #[test]
    fn user_tool_result_renders_web_fetch_and_search_like_official_tool_ui() {
        let fetch_raw = serde_json::json!({
            "bytes": 25600,
            "code": 200,
            "codeText": "OK",
            "result": "# Title\nBody",
            "durationMs": 10,
            "url": "https://example.com"
        });
        let fetch = render_tool_result_lines_for_result(
            "WebFetch",
            ToolResultStatus::Success,
            "Received 25 KB (200 OK)",
            Some(&fetch_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(fetch[0].text, "Received 25 KB (200 OK)");
        assert_eq!(fetch[0].tone, ToolRenderTone::Normal);
        assert_eq!(fetch.len(), 1);

        let verbose_fetch = render_tool_result_lines_for_result(
            "WebFetch",
            ToolResultStatus::Success,
            "Received 25 KB (200 OK)",
            Some(&fetch_raw),
            None,
            &[],
            ToolRenderOptions {
                verbose: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(verbose_fetch[1].text, "# Title");
        assert_eq!(verbose_fetch[2].text, "Body");

        let search = render_tool_result_lines_for_result(
            "WebSearch",
            ToolResultStatus::Success,
            "Did 2 searches in 2s",
            Some(&serde_json::json!({
                "query": "rust",
                "results": [
                    {"tool_use_id": "srvtoolu_1", "content": []},
                    {"tool_use_id": "srvtoolu_2", "content": []}
                ],
                "durationSeconds": 2.0
            })),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(search[0].text, "Did 2 searches in 2s");
        assert_eq!(search[0].tone, ToolRenderTone::Normal);
    }

    #[test]
    fn user_tool_result_renders_generic_mcp_result_like_official_tool_ui() {
        // The raw `toolUseResult` on the row drives the renderer.
        let empty_raw = serde_json::Value::String(String::new());
        let empty = render_tool_result_lines_for_result(
            "mcp__server__tool",
            ToolResultStatus::Success,
            "",
            Some(&empty_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(empty[0].text, "(No content)");
        assert_eq!(empty[0].tone, ToolRenderTone::Inactive);

        let text_raw = serde_json::Value::String("hello\nworld".to_string());
        let text = render_tool_result_lines_for_result(
            "mcp__server__tool",
            ToolResultStatus::Success,
            "hello\nworld",
            Some(&text_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(text.len(), 1);
        assert_eq!(text[0].text, "hello\nworld");
        assert_eq!(text[0].tone, ToolRenderTone::Normal);

        let large_raw = serde_json::Value::String("x".repeat(40_004));
        let large = render_tool_result_lines_for_result(
            "mcp__server__tool",
            ToolResultStatus::Success,
            "large",
            Some(&large_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(large[0].text.contains("Large MCP response"));
        assert_eq!(large[0].tone, ToolRenderTone::Warning);

        // The recorded live shape is the content-blocks array
        // (`data: mcpResult.content`, client.ts:1898).
        let blocks_raw = serde_json::json!([
            {"type": "text", "text": "hello"},
            {"type": "image", "source": {"type": "base64"}}
        ]);
        let blocks = render_tool_result_lines_for_result(
            "mcp__server__tool",
            ToolResultStatus::Success,
            "hello",
            Some(&blocks_raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(blocks.iter().any(|line| line.text.contains("hello")));
        assert!(blocks.iter().any(|line| line.text.contains("[Image]")));
    }

    /// The by-tool-name dispatch hands both MCP-resource tools the raw
    /// `toolUseResult` and each renderer parses it with its own schema.
    #[test]
    fn user_tool_result_renders_mcp_resource_results_like_official_tool_ui() {
        let empty_list = render_tool_result_lines_for_result(
            "ListMcpResources",
            ToolResultStatus::Success,
            "[]",
            Some(&serde_json::json!([])),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(empty_list[0].text, "(No resources found)");
        assert_eq!(empty_list[0].tone, ToolRenderTone::Inactive);

        let read_empty = render_tool_result_lines_for_result(
            "ReadMcpResource",
            ToolResultStatus::Success,
            "{}",
            Some(&serde_json::json!({"contents": []})),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert_eq!(read_empty[0].text, "(No content)");
        assert_eq!(read_empty[0].tone, ToolRenderTone::Inactive);

        let read_content = render_tool_result_lines_for_result(
            "ReadMcpResource",
            ToolResultStatus::Success,
            "{}",
            Some(&serde_json::json!({
                "contents": [{"uri": "res://a", "text": "hello"}]
            })),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(read_content[0].text.contains("hello"));
        assert_eq!(read_content[0].tone, ToolRenderTone::Normal);
    }

    #[test]
    fn user_tool_result_retired_remote_trigger_uses_generic_content() {
        // The raw `toolUseResult` on the row drives the renderer.
        let raw = serde_json::json!({
            "status": 202,
            "json": "{\n  \"ok\": true\n}",
        });
        let lines = render_tool_result_lines_for_result(
            "RemoteTrigger",
            ToolResultStatus::Success,
            "HTTP 202\n{\"ok\":true}",
            Some(&raw),
            None,
            &[],
            ToolRenderOptions::default(),
        );
        assert!(lines.iter().any(|line| line.text.contains("HTTP 202")));
        assert!(lines.iter().all(|line| !line.text.contains("(3 lines)")));
    }

    #[test]
    fn collapsed_write_error_uses_official_stable_summary() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Write".to_string(),
                    is_error: true,
                    content: "<tool_use_error>sensitive filesystem detail</tool_use_error>".to_string(),                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Error writing file"), "canvas=\n{text}");
        assert!(
            !text.contains("sensitive filesystem detail"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn collapsed_edit_error_uses_official_tool_specific_summary() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Edit".to_string(),
                    is_error: true,
                    content: "<tool_use_error>File has not been read yet. Read it first before writing to it.</tool_use_error>".to_string(),                    tool_use_result: Some(serde_json::Value::String(
                        "Error: File has not been read yet. Read it first before writing to it.".to_string(),
                    )),
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("File must be read first"), "canvas=\n{text}");
        assert!(
            !text.contains("File has not been read yet"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn glob_transcript_stays_collapsed_and_uses_five_column_search_gutter() {
        // Glob renders from the raw `toolUseResult` on the row.
        let raw = serde_json::json!({
            "durationMs": 1,
            "numFiles": 1,
            "filenames": ["src/private-detail.rs"],
            "truncated": false
        });
        let success = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Glob".to_string(),
                    is_error: false,
                    content: "src/private-detail.rs".to_string(),                    tool_use_result: Some(raw),
                    verbose: false,
                    is_transcript_mode: true,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(success.contains("  ⎿  Found 1 file"), "canvas=\n{success}");
        assert!(!success.contains("private-detail"), "canvas=\n{success}");

        let error = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Glob".to_string(),
                    is_error: true,
                    content: "<tool_use_error>sensitive detail</tool_use_error>".to_string(),                    verbose: false,
                    is_transcript_mode: true,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(error.contains("Error searching files"), "canvas=\n{error}");
        assert!(!error.contains("sensitive detail"), "canvas=\n{error}");
    }

    #[test]
    fn read_cancel_reject_and_error_routing_matches_official() {
        for control_message in [
            crate::utils::messages::CANCEL_MESSAGE,
            crate::utils::messages::REJECT_MESSAGE,
        ] {
            let canvas = element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    UserToolResultMessage(
                        tool_name: "Read".to_string(),
                        is_error: true,
                        content: control_message.to_string(),                        verbose: false,
                        is_transcript_mode: false,
                    )
                }
            }
            .render(None)
            .to_string();
            assert!(
                canvas.contains("Interrupted · What should Claude do instead?"),
                "canvas=\n{canvas}"
            );
            assert!(
                !canvas.contains("STOP what you are doing"),
                "canvas=\n{canvas}"
            );
        }

        let missing = "File does not exist. Note: your current working directory is /tmp.";
        let compact = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Read".to_string(),
                    is_error: true,
                    content: missing.to_string(),                    verbose: false,
                    is_transcript_mode: true,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(compact.contains("File not found"), "canvas=\n{compact}");
        assert!(!compact.contains("current working directory"));

        let fallback = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Read".to_string(),
                    is_error: true,
                    content: "disk exploded".to_string(),                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(
            fallback.contains("Error: disk exploded"),
            "canvas=\n{fallback}"
        );

        let rejected_with_reason = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Read".to_string(),
                    is_error: true,
                    content: format!(
                        "{}use a safer file",
                        crate::utils::messages::REJECT_MESSAGE_WITH_REASON_PREFIX
                    ),                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(
            rejected_with_reason.contains("Tool use rejected"),
            "canvas=\n{rejected_with_reason}"
        );
        assert!(!rejected_with_reason.contains("use a safer file"));
    }

    #[test]
    fn read_error_row_dispatch_matches_official_decision_and_fallback() {
        // Read errors dispatch by tool name (CC's error leaf renders from
        // content alone).
        let missing = "File does not exist. Note: your current working directory is /tmp.";

        let compact = render_tool_result_lines("Read", ToolResultStatus::Error, missing);
        assert_eq!(compact[0].text, "File not found");
        assert_eq!(compact[0].tone, ToolRenderTone::Error);

        let verbose = render_tool_result_lines_with_options(
            "Read",
            ToolResultStatus::Error,
            missing,
            ToolRenderOptions {
                verbose: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(verbose[0].text, format!("Error: {missing}"));

        let transcript = render_tool_result_lines_with_options(
            "Read",
            ToolResultStatus::Error,
            missing,
            ToolRenderOptions {
                is_transcript_mode: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(transcript[0].text, "File not found");

        let tagged = render_tool_result_lines(
            "Read",
            ToolResultStatus::Error,
            "<tool_use_error>boom</tool_use_error>",
        );
        assert_eq!(tagged[0].text, "Error reading file");

        let long = (0..12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let collapsed = render_tool_result_lines("Read", ToolResultStatus::Error, &long);
        assert_eq!(collapsed.len(), 11);
        assert_eq!(collapsed[0].text, "Error: line 0");
        assert_eq!(collapsed[10].text, "… +2 lines (ctrl+o to see all)");
        assert!(collapsed[10].dim);

        let transcript_fallback = render_tool_result_lines_with_options(
            "Read",
            ToolResultStatus::Error,
            &long,
            ToolRenderOptions {
                is_transcript_mode: true,
                ..ToolRenderOptions::default()
            },
        );
        assert_eq!(transcript_fallback.len(), 11);
        assert_eq!(
            transcript_fallback[10].text,
            "… +2 lines (ctrl+o to see all)"
        );
    }

    #[test]
    fn user_tool_result_assistant_text_display_omits_tool_result_gutter() {
        // SendUserMessage renders the Brief component from the raw
        // `toolUseResult` — like CC's userFacingName-'' tools it reads as
        // plain assistant text, with no ⎿ gutter or tool chrome.
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "SendUserMessage".to_string(),
                    is_error: false,
                    content: "Message delivered to user.".to_string(),                    tool_use_result: Some(serde_json::json!({"message": "Hello **there**"})),
                    verbose: true,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Hello"), "canvas=\n{text}");
        assert!(!text.contains("⎿"), "canvas=\n{text}");
        assert!(!text.contains("SendUserMessage"), "canvas=\n{text}");
    }

    #[test]
    fn user_tool_result_renders_bash_output_through_ansi_boundary() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Bash".to_string(),
                    is_error: false,
                    content: String::new(),                    tool_use_result: Some(serde_json::json!({
                        "stdout": "\x1b[31mred\x1b[0m plain",
                        "stderr": "",
                        "interrupted": false,
                        "noOutputExpected": false
                    })),
                    verbose: true,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);

        assert!(canvas.to_string().contains("red plain"));
        assert!((0..20).any(|x| {
            (0..2).any(|y| {
                canvas
                    .resolved_text_style(x, y)
                    .is_some_and(|style| style.color == Some(Color::DarkRed))
            })
        }));
    }

    #[test]
    fn user_tool_result_diff_lines_use_structured_diff_backgrounds() {
        let theme = *crate::utils::theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                // Renders StructuredDiffList, which reads
                // `settings.syntax_highlighting_disabled` (strict since P7).
                // Default state is the fixture: this asserts on diff
                // backgrounds, which are emitted either way.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                UserToolResultMessage(
                    tool_name: "Edit".to_string(),
                    is_error: false,
                    content: String::new(),                    tool_use_result: Some(serde_json::json!({
                        "filePath": "src/lib.rs",
                        "oldString": "old",
                        "newString": "new",
                        "originalFile": "old",
                        "structuredPatch": [{
                            "oldStart": 10, "oldLines": 1, "newStart": 10, "newLines": 1,
                            "lines": ["-old", "+new"]
                        }],
                        "userModified": false,
                        "replaceAll": false
                    })),
                    verbose: true,
                    is_transcript_mode: false,
                )
                }.into_any()),
                )
            }
        }
        .render(None);

        assert!(canvas.to_string().contains("10 -old"));
        // Success rendering now delegates through the official
        // FileEditToolUpdatedMessage → StructuredDiffList component boundary;
        // that boundary is stable text in Nyan's main-screen renderer.
        assert!(canvas.to_string().contains("10 -old"));
        assert!(canvas.to_string().contains("10 +new"));
    }

    #[test]
    fn successful_plan_edit_uses_official_plan_preview_hint() {
        let path = crate::utils::plans::get_plans_directory().join("plan.md");
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "Edit".to_string(),
                    is_error: false,
                    content: String::new(),                    tool_use_result: Some(serde_json::json!({
                        "filePath": path.display().to_string(),
                        "oldString": "old",
                        "newString": "new",
                        "originalFile": "old",
                        "structuredPatch": [{
                            "oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                            "lines": ["-old", "+new"]
                        }],
                        "userModified": false,
                        "replaceAll": false
                    })),
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(canvas.contains("/plan to preview"), "canvas=\n{canvas}");
        assert!(!canvas.contains("-old"), "canvas=\n{canvas}");
    }

    #[test]
    fn user_tool_result_official_file_edit_rejected_component_boundary() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                // Same StructuredDiffList read as the success case above.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                UserToolResultMessage(
                    tool_name: "Edit".to_string(),
                    is_error: true,
                    // CC wire: rejected results carry the REJECT sentinel in
                    // content — that is what routes to the reject leaf now.
                    content: crate::utils::messages::REJECT_MESSAGE.to_string(),                    // The rejected preview computes from the tool INPUT
                    // at render time; the missing file falls back to the
                    // inputs-only diff (-old/+new).
                    tool_input: Some(serde_json::json!({
                        "file_path": "src/lib.rs",
                        "old_string": "old",
                        "new_string": "new",
                        "replace_all": false
                    })),
                    verbose: true,
                    is_transcript_mode: false,
                )
                }.into_any()),
                )
            }
        }
        .render(None)
        .to_string();

        assert!(
            canvas.contains("User rejected update to src/lib.rs"),
            "canvas=\n{canvas}"
        );
        assert!(canvas.contains("-old"), "canvas=\n{canvas}");
        assert!(canvas.contains("+new"), "canvas=\n{canvas}");
    }

    /// The rejected leaf renders from the tool INPUT via
    /// `UserToolRejectMessage` (`UserToolRejectMessage.tsx:46`); the display
    /// carries no NotebookEdit shape.
    #[test]
    fn user_tool_result_official_notebook_rejected_component_boundary() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UserToolResultMessage(
                    tool_name: "NotebookEdit".to_string(),
                    is_error: true,
                    // CC wire: rejected results carry the REJECT sentinel in
                    // content — that is what routes to the reject leaf now.
                    content: crate::utils::messages::REJECT_MESSAGE.to_string(),                    tool_input: Some(serde_json::json!({
                        "notebook_path": "notebooks/demo.ipynb",
                        "cell_id": "cell-1",
                        "new_source": "print('hello')",
                        "cell_type": "code",
                        "edit_mode": "replace"
                    })),
                    verbose: true,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(
            canvas.contains("User rejected replace cell in notebooks/demo.ipynb at cell cell-1"),
            "canvas=\n{canvas}"
        );
        assert!(canvas.contains("print('hello')"), "canvas=\n{canvas}");
    }

    #[test]
    fn user_tool_result_ansi_output_inherits_error_color_after_sgr_reset() {
        let theme = *crate::utils::theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                UserToolResultMessage(
                    tool_name: "Bash".to_string(),
                    is_error: true,
                    // Error rendering consumes the model-facing tool_result
                    // content, not the success-only raw `toolUseResult`.
                    content: "\x1b[32mgreen\x1b[0m plain".to_string(),                    tool_use_result: Some(serde_json::Value::String(
                        "\x1b[32mgreen\x1b[0m plain".to_string(),
                    )),
                    verbose: true,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);

        let text = canvas.to_string();
        assert!(text.contains("Error: green plain"), "canvas=\n{text:?}");
        assert!(
            canvas
                .resolved_text_style(11, 0)
                .is_some_and(|style| style.color == Some(Color::DarkGreen))
        );
        assert!(
            canvas
                .resolved_text_style(17, 0)
                .is_some_and(|style| style.color == Some(theme.error))
        );
    }
}
