//! Maps to: CC `components/messages/AssistantToolUseMessage.tsx`.

use crate::components::design_system::progress_bar::progress_bar_text;
use crate::components::file_path_link::file_path_link_segment;
use crate::components::message_response::MessageResponse;
use crate::components::messages::user_tool_result_message::utils::ToolRenderOptions;
use crate::components::tool_use_loader::ToolUseLoader;
use crate::constants::figures::BLACK_CIRCLE;
use crate::tools::agent_tool::ui::AgentToolUseProgressMessage;
use crate::tools::{bash_tool, file_read_tool, powershell_tool};
use crate::types::message::{ToolUseProgressMessage, ToolUseStatus};
use crate::utils::classifier_approvals::ClassifierApprovalsState;
use crate::utils::classifier_approvals_hook::use_is_classifier_checking;
use iocraft::prelude::*;
use unicode_width::UnicodeWidthStr;

/// Maps to: CC `AssistantToolUseMessage.tsx:120-121`.
///
/// ```ts
/// const isResolved = lookups.resolvedToolUseIDs.has(param.id)
/// const isQueued = !inProgressToolUseIDs.has(param.id) && !isResolved
/// ```
///
/// A tool use has no status of its own — it is a position in two sets. The set
/// of live tool uses is REPL state (`REPL.tsx:1897`); `resolvedToolUseIDs` and
/// `erroredToolUseIDs` come from `buildMessageLookups`, i.e. from whether a
/// matching tool_result has arrived. Cometix used to store the result of this
/// derivation on the row instead, which made the transcript the authority and
/// forced the query actor to re-emit a whole row per transition.
pub fn derive_tool_use_status(
    tool_use_id: Option<&str>,
    in_progress_tool_use_ids: &std::collections::HashSet<String>,
    lookups: Option<&crate::components::messages_list::MessageLookups>,
) -> ToolUseStatus {
    // No id: a recovered/mock row that can never be matched to a result. CC
    // renders these through the same `isQueued` branch.
    let Some(tool_use_id) = tool_use_id else {
        return ToolUseStatus::Queued;
    };
    if let Some(lookups) = lookups {
        if lookups.resolved_tool_use_ids.contains(tool_use_id) {
            return if lookups.errored_tool_use_ids.contains(tool_use_id) {
                ToolUseStatus::Failed
            } else {
                ToolUseStatus::Succeeded
            };
        }
    }
    if in_progress_tool_use_ids.contains(tool_use_id) {
        ToolUseStatus::Running
    } else {
        ToolUseStatus::Queued
    }
}

#[derive(Default, Props)]
pub struct AssistantToolUseMessageProps {
    pub tool_name: String,
    /// Official `param.input`. When present the row renders through the owning
    /// tool's `renderToolUseMessage`; `description` is only the fallback for
    /// rows recovered without a `ToolUseBlock`.
    pub input: Option<serde_json::Value>,
    pub description: String,
    pub status: Option<ToolUseStatus>,
    /// Official `param.id` equivalent. Mock message rows fall back to their
    /// message id before reaching this component.
    pub tool_use_id: Option<String>,
    pub add_margin: bool,
    /// Mirrors official MessageRow's animation gate. Running tool rows stay
    /// visible in native scrollback, but their loader only animates while the
    /// row is in the live loading set.
    pub can_animate: bool,
    /// Maps to official `shouldShowDot`. Most main-screen rows show the dot;
    /// specialized tool-renderer embeddings can opt out without changing the
    /// tool name/message layout contract.
    pub should_show_dot: Option<bool>,
    /// UI-only typed progress seam for official `ProgressMessage<ToolProgressData>`
    /// rows. Mock/main-screen paths can provide already-known progress snapshots
    /// without starting real tools or progress streams.
    pub progress_messages: Vec<ToolUseProgressMessage>,
    /// Mirrors official `tool.isTransparentWrapper()`: wrapper tools hide their
    /// own chrome and surface only the running progress UI.
    pub is_transparent_wrapper: bool,
    pub verbose: bool,
    pub is_transcript_mode: bool,
    /// Maps to CC `inProgressToolCallCount` (`AssistantToolUseMessage.tsx:36`),
    /// produced at the Message mount as `inProgressToolUseIDs.size`
    /// (`Message.tsx:129`). Feeds AgentTool's condensed-mode line estimate
    /// (`AgentTool/UI.tsx:542-544`); consumers apply CC's `?? 1`.
    pub in_progress_tool_call_count: Option<usize>,
}

/// Maps to: CC `components/messages/AssistantToolUseMessage.tsx:41-216`
/// `AssistantToolUseMessage`.
#[component]
pub fn AssistantToolUseMessage(
    props: &AssistantToolUseMessageProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let status = props.status.unwrap_or(ToolUseStatus::Queued);
    let should_animate = tool_use_loader_should_animate(status, props.can_animate);
    // Maps to CC `AssistantToolUseMessage.tsx:55` `useTerminalSize()` —
    // unconditional and before every early return so the hook order stays
    // stable across renders (this hook holds state, unlike the context reads
    // below).
    let (_, terminal_rows) = hooks.use_terminal_size();
    // Maps to CC `AssistantToolUseMessage.tsx:58-60,122`: the row reads
    // `pendingWorkerRequest` itself — a swarm worker waiting on its leader's
    // approval for this tool use — rather than being told by its parents.
    // Unconditional for the same hook-order reason as above.
    let pending_worker_request =
        crate::state::app_state::use_app_state_maybe_outside_of_provider(&mut hooks, |state| {
            state.pending_worker_request.clone()
        })
        .flatten();
    let is_waiting_for_permission = pending_worker_request
        .as_ref()
        .is_some_and(|request| props.tool_use_id.as_deref() == Some(request.tool_use_id.as_str()));
    let progress_options = ToolUseProgressOptions {
        verbose: props.verbose,
        is_transcript_mode: props.is_transcript_mode,
        terminal_rows: Some(terminal_rows),
        in_progress_tool_call_count: props.in_progress_tool_call_count,
    };
    if !assistant_tool_use_should_render(&props.tool_name) {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }
    // Maps to CC `AssistantToolUseMessage.tsx` `findToolByName(...)`: resolve
    // the live pseudo-tool object rather than reverse-parsing its generated MCP
    // name or recreating its presentation closures in this consumer.
    let mcp_auth_tool = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .and_then(|store| {
            let state = store.get();
            let (server_name, raw_tool_name) =
                crate::services::mcp::client::resolve_mcp_tool_invocation(
                    &props.tool_name,
                    &state.mcp,
                )?;
            if raw_tool_name != "authenticate" {
                return None;
            }
            let server = state.mcp.clients.iter().find(|server| {
                server.client.name == server_name
                    && server.client.status
                        == crate::services::mcp::types::McpServerConnectionType::NeedsAuth
            })?;
            let config = server.config.as_ref()?;
            Some(crate::tools::mcp_auth_tool::create_mcp_auth_tool(
                &server_name,
                config,
            ))
        });
    if props.is_transparent_wrapper {
        return assistant_transparent_wrapper_row(
            &props.tool_name,
            status,
            &props.progress_messages,
            progress_options,
        );
    }

    // Maps to CC `AssistantToolUseMessage.tsx:145-150`: `input.success` is the
    // `safeParse` from `:88`, and a failed parse leaves `renderedToolUseMessage`
    // null, which hides the row before any tool renderer runs.
    //
    // The position is CC's, not convenience. `isTransparentWrapper` is checked
    // FIRST (`:124-139`), so a wrapper row still shows its progress UI on an
    // input its schema rejects; and `:141`'s empty-facing-name return — Rust's
    // `assistant_tool_use_should_render` above — comes one line before this one,
    // so a nonvisual tool is hidden by that branch whatever its input does.
    if !assistant_tool_use_input_parses(&props.tool_name, props.input.as_ref()) {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    // Maps to CC `AssistantToolUseMessage`: the row hands `param.input` to the
    // owning tool's `renderToolUseMessage` on every render, so the summary
    // tracks `verbose`. A `null` result hides the whole row. Rows recovered
    // without a `ToolUseBlock` fall back to their pre-rendered summary.
    let raw_description = match props.input.as_ref() {
        Some(_input) if mcp_auth_tool.is_some() => mcp_auth_tool
            .as_ref()
            .expect("checked MCP auth tool")
            .render_tool_use_message()
            .to_string(),
        Some(input) => match render_tool_use_message(
            &props.tool_name,
            input,
            ToolRenderOptions {
                verbose: props.verbose,
                is_transcript_mode: props.is_transcript_mode,
                ..ToolRenderOptions::default()
            },
        ) {
            Some(rendered) => rendered,
            None => return element! { View(width: 0u32, height: 0u32) }.into_any(),
        },
        None => props.description.clone(),
    };
    let tool_use_tag = assistant_tool_use_tag(&props.tool_name, props.input.as_ref());
    // CC composes the row from two INDEPENDENT sources and never derives one
    // from the other: the rendered summary (`AssistantToolUseMessage.tsx:145`)
    // and `tool.renderToolUseTag(input.data)` (`:199-201`).
    let display_parts = AssistantToolUseDisplayParts {
        description: raw_description,
        tag: tool_use_tag,
    };
    let read_path_link = if props.tool_name.eq_ignore_ascii_case("Read") {
        props.input.as_ref().and_then(|input| {
            file_read_tool::ui::render_tool_use_path_link(
                input,
                props.verbose,
                &display_parts.description,
            )
        })
    } else {
        None
    };
    // Edit/Write render their header path inside FilePathLink too
    // (FileEditTool/UI.tsx:82-86, FileWriteTool/UI.tsx:130-135); they carry
    // no suffix.
    let file_tool_path_link = match props.tool_name.to_ascii_lowercase().as_str() {
        "edit" | "multiedit" | "fileedit" => props.input.as_ref().and_then(|input| {
            crate::tools::file_edit_tool::ui::render_tool_use_path_link(input, props.verbose)
        }),
        "write" | "filewrite" => props.input.as_ref().and_then(|input| {
            crate::tools::file_write_tool::ui::render_tool_use_path_link(input, props.verbose)
        }),
        _ => None,
    };
    // NotebookEdit links the path too, but keeps the `@cell_id…` remainder
    // outside the link (NotebookEditTool/UI.tsx:41-53).
    let notebook_path_link = if props.tool_name.eq_ignore_ascii_case("NotebookEdit") {
        props.input.as_ref().and_then(|input| {
            let (path, label) = crate::tools::notebook_edit_tool::ui::render_tool_use_path_link(
                input,
                props.verbose,
            )?;
            let suffix = crate::tools::notebook_edit_tool::ui::render_tool_use_message_suffix(
                input,
                props.verbose,
            )?;
            Some((path, label, suffix))
        })
    } else {
        None
    };
    // Maps to CC `tool.userFacingName(input.data)` for every live row. Keep
    // the internal tool name for lookup while rendering the resolved facing
    // name (notably Grep/Glob → Search). This is the row's ONLY name source:
    // CC never lets a rendered summary override it.
    let user_facing_name = mcp_auth_tool
        .as_ref()
        .map(|tool| tool.user_facing_name().to_string())
        .unwrap_or_else(|| tool_use_display_name(&props.tool_name, props.input.as_ref()));
    let display_tool_name = user_facing_name.as_str();
    let is_unresolved = matches!(status, ToolUseStatus::Queued | ToolUseStatus::Running);
    let is_error = status == ToolUseStatus::Failed;
    let should_show_dot = props.should_show_dot.unwrap_or(true);
    let tool_name_min_width = tool_name_min_width(display_tool_name, should_show_dot);
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    // Maps to CC `AssistantToolUseMessage.tsx:94-95`
    // `tool.userFacingNameBackgroundColor?.(data)` — resolved from the SAME
    // input the facing name came from, never reverse-parsed out of the rendered
    // summary. `AgentTool` is the only tool in CC 2.1.88 that implements the
    // optional member (`AgentTool.tsx:131`, `:1688`).
    let agent_background_color =
        tool_use_display_name_background_color(&props.tool_name, props.input.as_ref())
            .map(|color| theme.color(color));
    let agent_text_color = agent_background_color.map(|_| theme.inverse_text);
    let classifier_approvals = hooks
        .try_use_context::<ClassifierApprovalsState>()
        .map(|state| (*state).clone())
        .unwrap_or_default();
    let is_classifier_checking = props.tool_use_id.as_deref().is_some_and(|id| {
        classifier_approvals.is_classifier_checking(id) || use_is_classifier_checking(&hooks, id)
    });

    // CC `:199-231` renders ONE progress slot. Two Rust projections of it: a
    // mounted component where the tool's CC renderer is a component tree
    // (AgentTool), lines otherwise. The element half runs first and, when it
    // answers, owns the slot outright.
    let auxiliary_element = tool_use_auxiliary_element(
        &props.tool_name,
        status,
        is_classifier_checking,
        is_waiting_for_permission,
        &props.progress_messages,
        progress_options,
    );
    let auxiliary_content = if auxiliary_element.is_some() {
        None
    } else {
        let auxiliary_messages = tool_use_auxiliary_messages(
            &props.tool_name,
            status,
            is_classifier_checking,
            classifier_approvals.checking_is_auto(),
            is_waiting_for_permission,
            &props.progress_messages,
            progress_options,
        );
        if auxiliary_messages.is_empty() {
            None
        } else {
            Some(auxiliary_messages.join("\n"))
        }
    };

    element! {
        View(
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SPACE_BETWEEN,
            margin_top: if props.add_margin { 1u32 } else { 0u32 },
            width: 100pct,
        ) {
            View(flex_direction: FlexDirection::Column) {
                View(
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::NoWrap,
                    min_width: tool_name_min_width,
                ) {
                    #(if !should_show_dot {
                        None
                    } else if status == ToolUseStatus::Queued {
                        // CC queued branch: `<Text dimColor={isQueued}>{BLACK_CIRCLE}</Text>`
                        // (default fg + dim — not theme.inactive).
                        Some(element! {
                            View(min_width: 2u32, flex_shrink: 0.0f32) {
                                Text(content: BLACK_CIRCLE, dim: true, wrap: TextWrap::NoWrap)
                            }
                        }.into_any())
                    } else {
                        Some(element! {
                            ToolUseLoader(
                                should_animate: should_animate,
                                is_unresolved: is_unresolved,
                                is_error: is_error,
                            )
                        }.into_any())
                    })
                    View(flex_shrink: 0.0f32) {
                        Text(
                            content: display_tool_name.to_string(),
                            color: agent_text_color,
                            background_color: agent_background_color,
                            weight: Weight::Bold,
                            wrap: TextWrap::TruncateEnd,
                        )
                    }
                    // CC's squash emits suffix and ")" as separate unstyled
                    // segments; folding them into one segment here has no
                    // observable style, wrap, or link difference.
                    #(if display_parts.description.is_empty() {
                        None
                    } else if let Some(link) = read_path_link {
                        Some(element! {
                            View(flex_wrap: FlexWrap::NoWrap, flex_shrink: 1.0f32) {
                                Text(segments: Some(vec![
                                    StyledSegment::new("("),
                                    file_path_link_segment(
                                        &link.file_path,
                                        Some(link.label),
                                        None,
                                    ),
                                    StyledSegment::new(format!("{})", link.suffix)),
                                ]))
                            }
                        })
                    } else if let Some((link_path, link_label)) = file_tool_path_link {
                        Some(element! {
                            View(flex_wrap: FlexWrap::NoWrap, flex_shrink: 1.0f32) {
                                Text(segments: Some(vec![
                                    StyledSegment::new("("),
                                    file_path_link_segment(
                                        &link_path,
                                        Some(link_label),
                                        None,
                                    ),
                                    StyledSegment::new(")"),
                                ]))
                            }
                        })
                    } else if let Some((link_path, link_label, suffix)) = notebook_path_link {
                        Some(element! {
                            View(flex_wrap: FlexWrap::NoWrap, flex_shrink: 1.0f32) {
                                Text(segments: Some(vec![
                                    StyledSegment::new("("),
                                    file_path_link_segment(
                                        &link_path,
                                        Some(link_label),
                                        None,
                                    ),
                                    StyledSegment::new(format!("{suffix})")),
                                ]))
                            }
                        })
                    } else {
                        Some(element! {
                            View(flex_wrap: FlexWrap::NoWrap, flex_shrink: 1.0f32) {
                                Text(content: format!("({})", display_parts.description))
                            }
                        })
                    })
                    #(display_parts.tag.map(|tag| {
                        element! {
                            View(flex_wrap: FlexWrap::NoWrap, flex_shrink: 0.0f32) {
                                Text(content: format!(" {tag}"), dim: true, wrap: TextWrap::NoWrap)
                            }
                        }
                    }))
                }
                #(auxiliary_element)
                #(auxiliary_content.map(|message| {
                    element! { MessageResponse(content: message, color: None) }
                }))
            }
        }
    }
    .into_any()
}

/// The row's parenthesised text and its trailing dim tag — CC's
/// `renderedToolUseMessage` (`AssistantToolUseMessage.tsx:146`) and
/// `tool.renderToolUseTag(input.data)` (`:200`).
///
/// The row's bold NAME is not in here: it has exactly one source,
/// `tool.userFacingName(data)` (`:93`), and nothing may override it from the
/// rendered summary. A `tool_name` override field lived here until Cut E and
/// was the reverse-parse the Agent row used to disagree with the grouped
/// renderer through.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AssistantToolUseDisplayParts {
    description: String,
    tag: Option<String>,
}

/// Maps to CC `AssistantToolUseMessage.tsx:199-201`
/// `input.success && tool.renderToolUseTag && tool.renderToolUseTag(input.data)`
/// — the optional per-tool tag member, dispatched by tool name because Rust has
/// no tool-object registry at the render boundary.
///
/// Every arm reads the tool's own member off the raw input. Nothing here may
/// recover a tag from the rendered summary: that reverse-parse is what Cut E
/// removed from the Agent row and what TaskOutput carried until the tag arm
/// below existed.
fn assistant_tool_use_tag(tool_name: &str, input: Option<&serde_json::Value>) -> Option<String> {
    match tool_name.to_ascii_lowercase().as_str() {
        "read" => input.and_then(file_read_tool::ui::render_tool_use_tag),
        "agent" | "task" => input.and_then(crate::tools::agent_tool::ui::render_tool_use_tag),
        // `TaskOutputTool.tsx:377-382`. The name set is CC's canonical name plus
        // its two declared aliases (`:176` `['AgentOutputTool','BashOutputTool']`)
        // plus the three this port invented, matching what
        // `user_tool_result_message/mod.rs:1053-1059` already accepts — the two
        // render paths must agree on which rows are TaskOutput rows.
        "taskoutput" | "taskoutputtool" | "agentoutputtool" | "bashoutputtool" | "bashoutput"
        | "agentoutput" => input.and_then(crate::tools::task_output_tool::ui::render_tool_use_tag),
        _ => None,
    }
}

/// Maps to: CC `AssistantToolUseMessage.tsx:88` `tool.inputSchema.safeParse(param.input)`
/// — the row's one parse, whose failure hides the whole row at `:145-150`.
///
/// CC resolves the Tool object with `findToolByName` (`Tool.ts:348-360`: exact
/// `name`, or a member of `aliases`) and reads `inputSchema` off it. Rust has no
/// tool-object registry at the render boundary, so the lookup is this name
/// dispatch — the same seam and the same shape as
/// `user_tool_reject_message.rs#migrated_input_schema` (CC
/// `UserToolRejectMessage.tsx:40-43`) and
/// `tool_execution.rs#validate_tool_input_for_execution` (CC
/// `toolExecution.ts:615-676`).
///
/// The keys are the LOWERCASED CC wire names plus CC's aliases, and nothing
/// else. FOUR tools declare aliases in CC 2.1.88 — an `ast-grep scan` over
/// `src/` for `{kind: pair, has: {field: key, regex: '^aliases$'}}`, run once
/// per language, returns `AgentTool.tsx:374` (`constants.ts:3` = `'Task'`),
/// `TaskStopTool.ts:44` (`'KillShell'`),
/// `TaskOutputTool.tsx:176` (`'AgentOutputTool'`, `'BashOutputTool'`) and
/// `BriefTool.ts:138` (`prompt.ts:2` = `'Brief'`, for `'SendUserMessage'`).
/// (A plain `-p 'aliases: [$$$]'` matches NOTHING — an object property is a
/// `pair` node, not a standalone expression — and a `grep` for `aliases` runs
/// past any `head` window into PowerShell's shell-alias prose. Both misreads
/// happened here; the count was three until the scan above.)
///
/// Three of the four are in the table. `BriefTool` is not, and does not need to
/// be: its `userFacingName()` returns `''` (`BriefTool.ts:142-144`), so CC hides
/// that row at `:141-143` — one branch before the parse can matter — and so does
/// `assistant_tool_use_should_render`.
///
/// SEAM — `None` means "no gate", NOT "parse failed". Anything absent from this
/// table renders exactly as it did before. Three populations depend on that:
///
/// 1. **MCP tools.** CC's pool carries them (`REPL.tsx:3176-3179`
///    `assembleToolPool(state.toolPermissionContext, state.mcp.tools)`), so
///    `findToolByName` resolves an `mcp__server__tool` row to that server's tool
///    and gates it on that server's own schema. Rust has no per-server schema at
///    this boundary, and `MCPTool`'s placeholder (`MCPTool.ts:14`
///    `z.object({}).passthrough()`) is a different tool's schema, so gating them
///    would risk deleting MCP rows the transcript must keep.
/// 2. **The name aliases this port invented** for rows written by other
///    versions: `fileread`, `multiedit`, `fileedit`, `filewrite`, `search`,
///    `bashoutput`, `listmcpresources`, `readmcpresource`. None is a CC tool
///    name or a CC alias, so CC's `findToolByName` returns `undefined` and drops
///    those rows one step earlier (`:100-110`). Rust renders them; gating them
///    would apply a NEIGHBOUR's schema to an input CC never validated against
///    it. Left ungated on purpose — audit follow-up 11 (A) is reported, not
///    implemented.
/// 3. **Tools with no `input_schema()` carrier**, which reach the `_` arm.
///
/// One known over-reach, bounded and recorded rather than special-cased:
/// `collapsed_read_search_content.rs` mounts this component for its expanded
/// verbose entries, but the CC site it maps to renders the row INLINE
/// (`CollapsedReadSearchContent.tsx:81-105`) and, although it safeParses,
/// keeps the row on failure — only the summary and tag drop out. The
/// populations differ enough for that to be unobservable: `collapse_read_search`
/// records only Read/Grep/Glob uses whose result was a Success, so their inputs
/// already passed `validate_tool_input_for_execution`.
fn assistant_tool_use_input_schema(tool_name: &str) -> Option<&'static crate::utils::zod::Schema> {
    match tool_name.to_ascii_lowercase().as_str() {
        "read" => Some(crate::tools::file_read_tool::input_schema()),
        "bash" => Some(crate::tools::bash_tool::input_schema()),
        "powershell" => Some(crate::tools::powershell_tool::input_schema()),
        "edit" => Some(crate::tools::file_edit_tool::input_schema()),
        "write" => Some(crate::tools::file_write_tool::input_schema()),
        "notebookedit" => Some(crate::tools::notebook_edit_tool::input_schema()),
        "grep" => Some(crate::tools::grep_tool::input_schema()),
        "glob" => Some(crate::tools::glob_tool::input_schema()),
        "webfetch" => Some(crate::tools::web_fetch_tool::input_schema()),
        "websearch" => Some(crate::tools::web_search_tool::input_schema()),
        "agent" | "task" => Some(crate::tools::agent_tool::input_schema()),
        "skill" => Some(crate::tools::skill_tool::input_schema()),
        "sendmessage" => Some(crate::tools::send_message_tool::input_schema()),
        "config" => Some(crate::tools::config_tool::input_schema()),
        "structuredoutput" => Some(crate::tools::synthetic_output_tool::input_schema()),
        "taskoutput" | "agentoutputtool" | "bashoutputtool" => {
            Some(crate::tools::task_output_tool::input_schema())
        }
        "taskstop" | "killshell" => Some(crate::tools::task_stop_tool::input_schema()),
        "enterworktree" => Some(crate::tools::enter_worktree_tool::input_schema()),
        "exitworktree" => Some(crate::tools::exit_worktree_tool::input_schema()),
        "lsp" => Some(crate::tools::lsp_tool::input_schema()),
        "remotetrigger" => Some(crate::tools::remote_trigger_tool::input_schema()),
        "croncreate" => Some(crate::tools::schedule_cron_tool::cron_create_input_schema()),
        "crondelete" => Some(crate::tools::schedule_cron_tool::cron_delete_input_schema()),
        "cronlist" => Some(crate::tools::schedule_cron_tool::cron_list_input_schema()),
        "listmcpresourcestool" => Some(crate::tools::list_mcp_resources_tool::input_schema()),
        "readmcpresourcetool" => Some(crate::tools::read_mcp_resource_tool::input_schema()),
        _ => None,
    }
}

/// CC `tool.inputSchema.safeParse(input)` reduced to its VALUE rather than its
/// bit: `Some(parsedInput.data)` on success, `None` on failure — the exact
/// discriminant CC's callers spell as
/// `parsedInput.success ? parsedInput.data : undefined`
/// (`AssistantToolUseMessage.tsx:88-93`, `AgentTool/UI.tsx:1102-1114`).
///
/// The returned value is the PARSED data, not the raw input, so a caller sees
/// the same key set CC's `z.infer` hands the tool.
///
/// A tool absent from [`assistant_tool_use_input_schema`]'s table has no carrier
/// to check against; the raw input passes through unchanged, which is the same
/// "absent from the table ⇒ unchanged" rule that function documents.
pub(crate) fn assistant_tool_use_parsed_input(
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<serde_json::Value> {
    let Some(schema) = assistant_tool_use_input_schema(tool_name) else {
        return Some(input.clone());
    };
    crate::utils::zod::safe_parse(schema, input).ok()
}

/// CC `AssistantToolUseMessage.tsx:88-89` + `:145-150` reduced to the single bit
/// the row needs: does this input survive its tool's `inputSchema`?
///
/// Extracted rather than inlined so the dispatch is testable and so the "absent
/// from the table ⇒ unchanged" rule has one owner.
pub(crate) fn assistant_tool_use_input_parses(
    tool_name: &str,
    input: Option<&serde_json::Value>,
) -> bool {
    // A missing `param.input` is not a CC state — `ToolUseBlockParam.input`
    // always exists there, and `safeParse(undefined)` fails every object schema.
    // Here `None` means "row recovered without a `ToolUseBlock`", which renders
    // its pre-baked `description` and never reaches a tool renderer, so there is
    // nothing to validate.
    let Some(input) = input else {
        return true;
    };
    assistant_tool_use_parsed_input(tool_name, input).is_some()
}

fn assistant_tool_use_should_render(tool_name: &str) -> bool {
    // Official tools can opt out of tool chrome by returning an empty
    // userFacingName (e.g. ToolSearch → `userFacingName: () => ''` →
    // AssistantToolUseMessage returns null). Keep the same boundary.
    !tool_name.is_empty() && !assistant_tool_use_is_nonvisual(tool_name, None)
}

fn tool_name_min_width(tool_name: &str, should_show_dot: bool) -> u32 {
    UnicodeWidthStr::width(tool_name) as u32 + if should_show_dot { 2 } else { 0 }
}

fn tool_use_loader_should_animate(status: ToolUseStatus, can_animate: bool) -> bool {
    status == ToolUseStatus::Running && can_animate
}

/// Maps to: CC `Tool.renderToolUseProgressMessage`'s option bag
/// (`Tool.ts:625-637`) minus `tools` — the render inputs
/// `AssistantToolUseMessage.tsx:283-290` threads into each tool's progress
/// renderer: `{verbose, terminalSize, inProgressToolCallCount,
/// isTranscriptMode}`.
#[derive(Clone, Copy, Debug, Default)]
struct ToolUseProgressOptions {
    verbose: bool,
    is_transcript_mode: bool,
    /// CC `terminalSize?.rows`. `None` mirrors a caller without a live
    /// terminal — CC's error/rejected replay re-entry omits `terminalSize`
    /// entirely (`AgentTool/UI.tsx:755-759`, `:781-785`).
    terminal_rows: Option<u16>,
    /// CC `inProgressToolCallCount`, `Message.tsx:129`
    /// `inProgressToolUseIDs.size`. Consumers apply CC's `?? 1`
    /// (`AssistantToolUseMessage.tsx:288`, `AgentTool/UI.tsx:543`).
    in_progress_tool_call_count: Option<usize>,
}

fn assistant_transparent_wrapper_row(
    tool_name: &str,
    status: ToolUseStatus,
    progress_messages: &[ToolUseProgressMessage],
    options: ToolUseProgressOptions,
) -> AnyElement<'static> {
    if status != ToolUseStatus::Running {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    let normalized_tool_name = tool_name.trim().to_ascii_lowercase();
    // CC `:124-139` calls the SAME `renderToolUseProgressMessage` helper the
    // main branch does, so both halves of the dispatch are reachable here too.
    if let Some(element) = tool_use_progress_auxiliary_element(
        normalized_tool_name.as_str(),
        progress_messages,
        options,
    ) {
        return element! {
            View(flex_direction: FlexDirection::Column, width: 100pct) {
                #(Some(element))
            }
        }
        .into_any();
    }
    let messages = tool_use_progress_auxiliary_messages(
        normalized_tool_name.as_str(),
        progress_messages,
        options,
    )
    .unwrap_or_default();
    if messages.is_empty() {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            MessageResponse(content: messages.join("\n"), color: None)
        }
    }
    .into_any()
}

// ─── Recorded tool-use summary dispatch ───────────────────────────────────
// Maps to: CC `components/messages/AssistantToolUseMessage.tsx` — the
// facing-name/summary dispatch that renders an assistant `tool_use` row by
// delegating to each tool's `renderToolUseMessage`.

pub(crate) fn assistant_tool_use_is_nonvisual(
    tool_name: &str,
    input: Option<&serde_json::Value>,
) -> bool {
    let lower = tool_name.to_ascii_lowercase();
    match lower.as_str() {
        "taskcreate" => crate::tools::task_create_tool::ui::render_tool_use_message().is_none(),
        "taskupdate" => crate::tools::task_update_tool::ui::render_tool_use_message().is_none(),
        "tasklist" => crate::tools::task_list_tool::ui::render_tool_use_message().is_none(),
        "taskget" => crate::tools::task_get_tool::ui::render_tool_use_message().is_none(),
        "teamcreate" => crate::tools::team_create_tool::ui::user_facing_name().is_empty(),
        "teamdelete" => crate::tools::team_delete_tool::ui::user_facing_name().is_empty(),
        "sendusermessage" | "brief" => crate::tools::brief_tool::user_facing_name().is_empty(),
        "sendmessage" => {
            crate::tools::send_message_tool::ui::render_tool_use_message(input).is_none()
        }
        "notebookedit" => input.is_some_and(|input| {
            crate::tools::notebook_edit_tool::ui::render_tool_use_message(input, false).is_none()
        }),
        "structuredoutput" => {
            crate::tools::synthetic_output_tool::ui::render_tool_use_message(input).is_none()
        }
        "todowrite" | "toolsearch" | "askuserquestion" | "enterplanmode" | "exitplanmode" => true,
        // `FileRead` is not an official CC 2.1.88 tool identity.
        "fileread" => true,
        _ => {
            // These two are `renderToolUseMessage`-null conditions
            // (`ConfigTool/UI.tsx:7-8` `if (!input.setting) return null`, and
            // the same shape on readMcpResource's `server`/`uri`), which CC
            // applies at `AssistantToolUseMessage.tsx:145-149` — one layer
            // BELOW the `userFacingName === ''` check at `:141-143`. Neither
            // tool has an empty facing name; `ConfigTool.ts:83` returns
            // `'Config'`.
            //
            // A caller with no input to inspect is therefore asking the
            // `:141-143` question, and the answer must be `false`. Answering
            // `true` hid EVERY Config and readMcpResource row, because
            // `assistant_tool_use_should_render` passes `None`.
            let Some(input) = input else {
                return false;
            };
            // Both CC guards are JS truthiness — `!input.setting`
            // (`ConfigTool/UI.tsx:8`) and `!input.uri || !input.server`
            // (`ReadMcpResourceTool/UI.tsx:15`) — so an EMPTY string is falsy
            // and hides the row. `as_str().is_none()` alone kept `""` and
            // rendered `Getting ` where CC renders nothing. A non-string value
            // cannot reach here on the component path (both schemas declare
            // `z.string()` and `assistant_tool_use_input_parses` rejects
            // first); treating it as falsy is the safe answer for the callers
            // that have no gate.
            let missing = |key: &str| {
                input
                    .get(key)
                    .and_then(|value| value.as_str())
                    .is_none_or(str::is_empty)
            };
            (lower == "config" && missing("setting"))
                || (matches!(lower.as_str(), "readmcpresourcetool" | "readmcpresource")
                    && (missing("server") || missing("uri")))
        }
    }
}

pub(crate) fn tool_use_display_name(tool_name: &str, input: Option<&serde_json::Value>) -> String {
    let lower = tool_name.to_ascii_lowercase();
    match lower.as_str() {
        "edit" | "multiedit" | "fileedit" => {
            crate::tools::file_edit_tool::ui::user_facing_name(input)
        }
        "notebookedit" => crate::tools::notebook_edit_tool::ui::user_facing_name().to_string(),
        "grep" => "Search".to_string(),
        "glob" => crate::tools::glob_tool::ui::user_facing_name().to_string(),
        "websearch" => "Web Search".to_string(),
        "webfetch" => "Fetch".to_string(),
        // CC `AgentTool.tsx:130` hangs `UI.tsx:989-1011#userFacingName` on the
        // Tool object, so the ungrouped row and the grouped renderer
        // (`UI.tsx:874`) read the SAME derivation off the SAME input. Anything
        // recovered from the rendered summary instead diverges on
        // `subagent_type: "agent"` / `"task"` / `"General-Purpose"`.
        "agent" | "task" => crate::tools::agent_tool::ui::user_facing_name(input),
        // Same name set as the tag arm above (`TaskOutputTool.tsx:176`, :180).
        "taskoutput" | "taskoutputtool" | "agentoutputtool" | "bashoutputtool" | "bashoutput"
        | "agentoutput" => "Task Output".to_string(),
        "taskstop" | "killshell" => "Stop Task".to_string(),
        "enterworktree" => "Creating worktree".to_string(),
        "exitworktree" => "Exiting worktree".to_string(),
        "lsp" => "LSP".to_string(),
        "remotetrigger" => "RemoteTrigger".to_string(),
        "listmcpresourcestool" | "listmcpresources" => "listMcpResources".to_string(),
        "readmcpresourcetool" | "readmcpresource" => "readMcpResource".to_string(),
        "write" => crate::tools::file_write_tool::ui::user_facing_name(input),
        "read" => crate::tools::file_read_tool::ui::user_facing_name(input),
        "bash" => "Bash".to_string(),
        // CC ToolSearchTool.userFacingName: () => ''
        "toolsearch" | "todowrite" | "askuserquestion" | "enterplanmode" | "exitplanmode" => {
            String::new()
        }
        _ => tool_name.to_string(),
    }
}

/// Maps to: CC `Tool.ts:525-527` `userFacingNameBackgroundColor?` — the
/// OPTIONAL member `AssistantToolUseMessage.tsx:94-95` calls with `?.`, so a
/// tool that does not implement it contributes no background. `AgentTool`
/// (`AgentTool.tsx:131`, `:1688`) is its only implementer in CC 2.1.88.
pub(crate) fn tool_use_display_name_background_color(
    tool_name: &str,
    input: Option<&serde_json::Value>,
) -> Option<crate::utils::theme::ThemeColorKey> {
    match tool_name.to_ascii_lowercase().as_str() {
        "agent" | "task" => crate::tools::agent_tool::ui::user_facing_name_background_color(input),
        _ => None,
    }
}

/// Maps to: CC `AssistantToolUseMessage.tsx:238-258` `renderToolUseMessage` —
/// the local helper that hands a tool's structured input to that tool's own
/// renderer, wrapped in a `try`/`catch`.
///
/// CC's three outcomes are preserved:
/// - `None` mirrors `tool.renderToolUseMessage` returning `null`, which hides
///   the whole tool-use row at `:148-150`.
/// - `Some("")` mirrors the helper's own two `return ''` paths — a valid
///   renderer producing an empty summary, or the `catch` — so the tool name
///   renders without a parenthesised summary.
/// - `Some(text)` is the rendered summary.
///
/// The row's INPUT-SCHEMA gate is not here. CC parses once at `:88` and
/// short-circuits at `:145` (`input.success ? … : null`) before this helper is
/// called at all; `assistant_tool_use_input_parses` is that check, and only the
/// component applies it. This function's other callers (`message.rs`,
/// `grouped_tool_use_content.rs`, `bash_permission_request`) mirror CC sites
/// that do not parse either:
/// `ast-grep --lang tsx -p '$A.inputSchema.safeParse($$$)' src/components/`
/// (plus the same on `--lang ts`) yields ten sites, none of them
/// `GroupedToolUseContent.tsx` — which has no `safeParse` of any kind
/// (`ast-grep --lang tsx -p '$A.safeParse($$$)'` on that file: no matches) —
/// while `BashPermissionRequest.tsx:520` hands `BashTool.renderToolUseMessage` a
/// hand-built `{ command, description }`. The Read arm's local validation below
/// therefore stays: it is the only schema check those callers get.
///
/// Raw input JSON is never returned, so a tool without a usable summary degrades
/// to the empty string instead of leaking `{"file_path":…}` into the row.
pub(crate) fn render_tool_use_message(
    tool_name: &str,
    input: &serde_json::Value,
    options: ToolRenderOptions,
) -> Option<String> {
    match tool_name.to_ascii_lowercase().as_str() {
        "fileread" => return None,
        "read" => {
            let normalized = crate::tool::ToolCall::normalize_input(
                &crate::tools::file_read_tool::FileReadTool,
                input,
            );
            let schema = crate::tools::file_read_tool::file_read_tool_schema();
            crate::services::tools::tool_execution::validate_tool_input_against_schema(
                "Read",
                &normalized,
                &schema.input_schema,
            )
            .ok()?;
            return crate::tools::file_read_tool::ui::render_tool_use_message(
                &normalized,
                options.verbose,
            );
        }
        _ => {}
    }
    if assistant_tool_use_is_nonvisual(tool_name, Some(input)) {
        return None;
    }

    // Rust has no tool-object registry at the render boundary, so perform the
    // `findToolByName` lookup here and forward to the resolved tool renderer.
    let verbose = options.verbose;
    let lower = tool_name.to_ascii_lowercase();
    let summary = match lower.as_str() {
        "bash" => crate::components::messages::user_tool_result_message::utils::first_string(
            input,
            &["command"],
        )
        .map(|command| bash_tool::ui::render_tool_use_message(&command, options)),
        "powershell" => powershell_tool::ui::render_tool_use_message(
            crate::components::messages::user_tool_result_message::utils::first_string(
                input,
                &["command"],
            )
            .as_deref(),
            verbose,
        ),
        "read" | "fileread" => None,
        "edit" | "multiedit" | "fileedit" => {
            crate::tools::file_edit_tool::ui::render_tool_use_message(input, verbose)
        }
        "write" | "filewrite" => {
            crate::tools::file_write_tool::ui::render_tool_use_message(Some(input), verbose)
        }
        "notebookedit" => {
            crate::tools::notebook_edit_tool::ui::render_tool_use_message(input, verbose)
        }
        "grep" | "search" => crate::tools::grep_tool::ui::render_tool_use_message(input, verbose),
        "glob" => crate::tools::glob_tool::ui::render_tool_use_message(input, verbose),
        "webfetch" => crate::tools::web_fetch_tool::ui::render_tool_use_message(
            crate::components::messages::user_tool_result_message::utils::first_string(
                input,
                &["url"],
            )
            .as_deref(),
            crate::components::messages::user_tool_result_message::utils::first_string(
                input,
                &["prompt"],
            )
            .as_deref(),
            verbose,
        ),
        "websearch" => {
            // Maps to: CC `WebSearchTool/UI.tsx` `renderToolUseMessage` (verbose domains).
            let query = crate::components::messages::user_tool_result_message::utils::first_string(
                input,
                &["query"],
            );
            query.map(|query| {
                let mut message = format!("\"{query}\"");
                if let Some(domains) = input
                    .get("allowed_domains")
                    .and_then(|value| value.as_array())
                {
                    let joined = domains
                        .iter()
                        .filter_map(|value| value.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    if !joined.is_empty() {
                        message.push_str(&format!(", only allowing domains: {joined}"));
                    }
                }
                if let Some(domains) = input
                    .get("blocked_domains")
                    .and_then(|value| value.as_array())
                {
                    let joined = domains
                        .iter()
                        .filter_map(|value| value.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    if !joined.is_empty() {
                        message.push_str(&format!(", blocking domains: {joined}"));
                    }
                }
                message
            })
        }
        // CC `AgentTool/UI.tsx:472-483`: `if (!description || !prompt) return
        // null` — a malformed/partial Agent input hides the whole row rather
        // than degrading to a bare "Agent". Short-circuit here so the shared
        // empty-string fallback below (which exists for tools whose CC
        // renderer returns a string) cannot resurrect the row.
        "agent" | "task" => return crate::tools::agent_tool::ui::render_tool_use_message(input),
        "skill" => crate::tools::skill_tool::ui::skill_tool_use_summary(input),
        "sendmessage" => crate::tools::send_message_tool::ui::render_tool_use_message(Some(input)),
        "structuredoutput" => {
            crate::tools::synthetic_output_tool::ui::render_tool_use_message(Some(input))
        }
        "config" => crate::tools::config_tool::ui::config_tool_use_summary(input),
        // Same name set as the tag and facing-name arms (`TaskOutputTool.tsx:176`).
        "taskoutput" | "taskoutputtool" | "agentoutputtool" | "bashoutputtool" | "bashoutput"
        | "agentoutput" => crate::tools::task_output_tool::ui::task_output_tool_use_summary(input),
        "taskstop" | "killshell" => Some(String::new()),
        "enterworktree" => Some("Creating worktree…".to_string()),
        "exitworktree" => Some("Exiting worktree…".to_string()),
        "lsp" => crate::tools::lsp_tool::ui::lsp_tool_use_summary(input, verbose),
        "remotetrigger" => {
            crate::tools::remote_trigger_tool::ui::remote_trigger_tool_use_summary(input)
        }
        "croncreate" => crate::tools::schedule_cron_tool::ui::cron_create_tool_use_summary(input),
        "crondelete" => crate::components::messages::user_tool_result_message::utils::first_string(
            input,
            &["id"],
        ),
        "cronlist" => Some(String::new()),
        "listmcpresourcestool" | "listmcpresources" => {
            crate::tools::list_mcp_resources_tool::ui::list_mcp_resources_tool_use_summary(input)
        }
        "readmcpresourcetool" | "readmcpresource" => {
            crate::tools::read_mcp_resource_tool::ui::read_mcp_resource_tool_use_summary(input)
        }
        name if name == "mcp" || crate::services::mcp::utils::is_mcp_tool_name(name) => {
            crate::tools::mcp_tool::ui::render_tool_use_message(input, verbose)
        }
        _ => crate::components::messages::user_tool_result_message::utils::first_string(
            input,
            &["description", "command", "file_path", "path"],
        ),
    };
    Some(
        summary
            .or_else(|| {
                // Last-resort text extraction over `text`/`content` fields. The input is
                // never stringified: CC shows no summary rather than a raw JSON payload.
                let text =
                    crate::components::messages::user_tool_result_message::utils::value_to_text(
                        input,
                    );
                (!text.is_empty()).then(|| crate::utils::truncate::truncate(&text, 80, false))
            })
            .unwrap_or_default(),
    )
}

#[cfg(test)]
fn tool_use_auxiliary_message(
    tool_name: &str,
    status: ToolUseStatus,
    is_classifier_checking: bool,
    is_auto_classifier: bool,
    is_waiting_for_permission: bool,
    progress_messages: &[ToolUseProgressMessage],
) -> Option<String> {
    tool_use_auxiliary_messages(
        tool_name,
        status,
        is_classifier_checking,
        is_auto_classifier,
        is_waiting_for_permission,
        progress_messages,
        ToolUseProgressOptions::default(),
    )
    .into_iter()
    .next()
}

fn tool_use_auxiliary_messages(
    tool_name: &str,
    status: ToolUseStatus,
    is_classifier_checking: bool,
    is_auto_classifier: bool,
    is_waiting_for_permission: bool,
    progress_messages: &[ToolUseProgressMessage],
    options: ToolUseProgressOptions,
) -> Vec<String> {
    if is_classifier_checking && status == ToolUseStatus::Running {
        return vec![if is_auto_classifier {
            "Auto classifier checking…".to_string()
        } else {
            "Bash classifier checking…".to_string()
        }];
    }

    // CC `:204-215`: under `!isResolved && !isQueued`, i.e. a running row. A
    // queued row shows its queued message instead (`:232`).
    if is_waiting_for_permission && status == ToolUseStatus::Running {
        return vec!["Waiting for permission…".to_string()];
    }

    let normalized_tool_name = tool_name.trim().to_ascii_lowercase();

    if status == ToolUseStatus::Running {
        if let Some(messages) = tool_use_progress_auxiliary_messages(
            normalized_tool_name.as_str(),
            progress_messages,
            options,
        ) {
            return messages;
        }
    }

    // Maps to BashTool/UI.tsx and PowerShellTool/UI.tsx queued/progress
    // renderers for the no-progress main-screen subset.
    if matches!(normalized_tool_name.as_str(), "bash" | "powershell") {
        return match status {
            ToolUseStatus::Queued => vec!["Waiting…".to_string()],
            ToolUseStatus::Running => vec!["Running…".to_string()],
            _ => Vec::new(),
        };
    }

    if status != ToolUseStatus::Running {
        return Vec::new();
    }

    // Maps additional official tool progress renderers that have deterministic
    // no-progress output and do not require live tool streams.
    if matches!(normalized_tool_name.as_str(), "fetch" | "webfetch") {
        return vec!["Fetching…".to_string()];
    }
    // MCP has no arm here: its own renderer owns the no-progress `Running…`
    // (`MCPTool/UI.tsx:74-80`) and always answers, so a second copy on this
    // fallback could only ever diverge from it.
    if matches!(normalized_tool_name.as_str(), "task output" | "taskoutput") {
        return vec!["Waiting for task (esc to give additional instructions)".to_string()];
    }
    // `agent`/`task` used to share this arm: their renderer's own empty-list
    // return is the same `Initializing…` text (`AgentTool/UI.tsx:532-538`), and
    // the string dispatch above bailed on an empty list before reaching it.
    // Since the Agent renderer became a component it answers that case itself,
    // so a second copy here would render it twice.
    if normalized_tool_name == "skill" {
        return vec!["Initializing…".to_string()];
    }

    Vec::new()
}

/// The ELEMENT half of CC's `tool.renderToolUseProgressMessage?.()` dispatch
/// (`AssistantToolUseMessage.tsx:274-277`), beside the string half
/// [`tool_use_progress_auxiliary_messages`]. Neither is invented: CC's member
/// returns a React node for every tool, and this port renders some of those
/// nodes as lines and some as mounted components — the same two-projection
/// split `tools/agent_tool/ui.rs` already documents for
/// `render_tool_result_message` / `render_tool_result_lines`.
///
/// `None` means "no element renderer for this tool", so the caller falls
/// through to the string half. AgentTool answers ALWAYS, including for an
/// empty progress list: CC's function never returns null — `Initializing…`
/// (`AgentTool/UI.tsx:532-538`, `:645-651`), the condensed one-liner
/// (`:580-599`) and the outer `MessageResponse` (`:664-719`) are all its own
/// outputs.
fn tool_use_progress_auxiliary_element(
    normalized_tool_name: &str,
    progress_messages: &[ToolUseProgressMessage],
    options: ToolUseProgressOptions,
) -> Option<AnyElement<'static>> {
    if !matches!(normalized_tool_name, "agent" | "task") {
        return None;
    }
    Some(
        element! {
            AgentToolUseProgressMessage(
                progress_messages: progress_messages.to_vec(),
                verbose: options.verbose,
                is_transcript_mode: options.is_transcript_mode,
                terminal_rows: options.terminal_rows,
                in_progress_tool_call_count: options.in_progress_tool_call_count,
            )
        }
        .into_any(),
    )
}

/// The element-half entry with CC's gate order around it
/// (`AssistantToolUseMessage.tsx:199-231`): the classifier row and the
/// permission row PRE-EMPT the tool's own progress renderer, and the renderer
/// runs only while the row is unresolved and not queued (`status == Running`).
/// [`tool_use_auxiliary_messages`] applies the same three gates; whichever half
/// answers, only one does.
fn tool_use_auxiliary_element(
    tool_name: &str,
    status: ToolUseStatus,
    is_classifier_checking: bool,
    is_waiting_for_permission: bool,
    progress_messages: &[ToolUseProgressMessage],
    options: ToolUseProgressOptions,
) -> Option<AnyElement<'static>> {
    if status != ToolUseStatus::Running {
        return None;
    }
    if is_classifier_checking || is_waiting_for_permission {
        return None;
    }
    tool_use_progress_auxiliary_element(
        tool_name.trim().to_ascii_lowercase().as_str(),
        progress_messages,
        options,
    )
}

fn tool_use_progress_auxiliary_messages(
    normalized_tool_name: &str,
    progress_messages: &[ToolUseProgressMessage],
    options: ToolUseProgressOptions,
) -> Option<Vec<String>> {
    let verbose = options.verbose;

    // Maps to `tools/MCPTool/UI.tsx:69-113#renderToolUseProgressMessage` — the
    // UI reads only `progress`/`total`/`progressMessage` (`:82`); the wire's
    // `status`/`serverName`/`toolName`/`elapsedTimeMs` fields are not rendered.
    //
    // Always `Some`, and therefore ahead of the shared `.last()?` below: CC's
    // MCP renderer never returns null. A missing `lastProgress?.data` (`:74-80`)
    // and an `undefined` progress (`:84-90`) are its own `Running…` outputs, so
    // the empty list and a foreign progress variant (every MCP field would be
    // `undefined`) both belong to this renderer rather than to the caller's
    // no-progress fallback.
    if normalized_tool_name == "mcp"
        || crate::services::mcp::utils::is_mcp_tool_name(normalized_tool_name)
    {
        return Some(match progress_messages.last() {
            Some(ToolUseProgressMessage::McpProgress {
                progress,
                total,
                progress_message,
                ..
            }) => mcp_progress_auxiliary_messages(*progress, *total, progress_message.as_deref()),
            _ => vec!["Running…".to_string()],
        });
    }

    // CC picks the newest progress message (`BashTool/UI.tsx:140`,
    // `MCPTool/UI.tsx:72` — `.at(-1)`).
    let last = progress_messages.last()?;

    // Maps to `tools/WebSearchTool/UI.tsx#renderToolUseProgressMessage`.
    if matches!(normalized_tool_name, "web search" | "websearch") {
        return match last {
            ToolUseProgressMessage::QueryUpdate { query } => {
                Some(vec![format!("Searching: {query}")])
            }
            ToolUseProgressMessage::SearchResultsReceived {
                query,
                result_count,
            } => Some(vec![format!(
                "Found {result_count} results for \"{query}\""
            )]),
            _ => None,
        };
    }

    // Maps to `tools/BashTool/UI.tsx#renderToolUseProgressMessage` and
    // `tools/PowerShellTool/UI.tsx#renderToolUseProgressMessage`.
    if matches!(normalized_tool_name, "bash" | "powershell") {
        return match last {
            ToolUseProgressMessage::BashProgress {
                output,
                full_output,
                elapsed_time_seconds,
                total_lines,
                total_bytes,
                task_id,
                timeout_ms,
            } => Some(shell_progress_auxiliary_messages(
                output,
                full_output,
                *elapsed_time_seconds,
                *total_lines,
                *total_bytes,
                task_id.as_deref(),
                *timeout_ms,
                verbose,
            )),
            _ => None,
        };
    }

    // Maps to `tools/TaskOutputTool/TaskOutputTool.tsx#renderToolUseProgressMessage`.
    if matches!(normalized_tool_name, "task output" | "taskoutput") {
        return match last {
            ToolUseProgressMessage::WaitingForTask {
                task_description,
                task_type: _,
            } => Some(task_output_progress_auxiliary_messages(task_description)),
            _ => None,
        };
    }

    // AgentTool has no arm here: its renderer is a component, not lines —
    // see [`tool_use_progress_auxiliary_element`], which runs BEFORE this
    // dispatch.

    // Maps to `tools/SkillTool/UI.tsx:75-135#renderToolUseProgressMessage`:
    // display the last few already-known skill progress messages without
    // starting a skill, tool registry, or progress stream.
    if normalized_tool_name == "skill" {
        let messages = skill_progress_auxiliary_messages(progress_messages, verbose);
        if !messages.is_empty() {
            return Some(messages);
        }
    }

    None
}

fn shell_progress_auxiliary_messages(
    output: &str,
    full_output: &str,
    elapsed_time_seconds: u64,
    total_lines: usize,
    total_bytes: Option<u64>,
    task_id: Option<&str>,
    timeout_ms: Option<u64>,
    verbose: bool,
) -> Vec<String> {
    let stripped_full_output = strip_ansi_sequences(full_output.trim());
    let stripped_output = strip_ansi_sequences(output.trim());
    let lines: Vec<&str> = stripped_output
        .split('\n')
        .filter(|line| !line.is_empty())
        .collect();
    let time_display = shell_time_display(Some(elapsed_time_seconds), timeout_ms);

    if lines.is_empty() {
        let mut messages = vec![join_non_empty(
            [Some("Running…".to_string()), time_display].into_iter(),
        )];
        if let Some(hint) = shell_background_hint(task_id) {
            messages.push(hint);
        }
        return messages;
    }

    let display_lines = if verbose {
        stripped_full_output
    } else {
        lines
            .iter()
            .skip(lines.len().saturating_sub(5))
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    };

    // CC ShellProgressMessage.tsx:52-60 — JS truthiness: `totalBytes &&
    // totalLines` treats 0 like undefined, so gate on > 0 here.
    let truthy_total_bytes = total_bytes.filter(|bytes| *bytes > 0);
    let extra_lines = total_lines.saturating_sub(5);
    let line_status = if !verbose && truthy_total_bytes.is_some() && total_lines > 0 {
        Some(format!("~{total_lines} lines"))
    } else if !verbose && extra_lines > 0 {
        Some(format!("+{extra_lines} lines"))
    } else {
        None
    };
    let byte_status = truthy_total_bytes.map(format_shell_file_size);
    let status_line = join_non_empty([line_status, time_display, byte_status].into_iter());

    let mut messages = if status_line.is_empty() {
        vec![display_lines]
    } else {
        vec![display_lines, status_line]
    };
    if let Some(hint) = shell_background_hint(task_id) {
        messages.push(hint);
    }
    messages
}

fn shell_background_hint(task_id: Option<&str>) -> Option<String> {
    if task_id.is_none()
        || crate::utils::env_utils::is_env_truthy(
            std::env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS")
                .ok()
                .as_deref(),
        )
    {
        return None;
    }
    let base_shortcut = crate::keybindings::shortcut_format::get_shortcut_display_for_context_name(
        "task:background",
        "Task",
        "ctrl+b",
    );
    let shortcut = if crate::utils::env::get().terminal.as_deref() == Some("tmux")
        && base_shortcut == "ctrl+b"
    {
        "ctrl+b ctrl+b (twice)".to_string()
    } else {
        base_shortcut
    };
    Some(format!("({shortcut} to run in background)"))
}

fn task_output_progress_auxiliary_messages(task_description: &str) -> Vec<String> {
    let mut messages = Vec::new();
    if !task_description.is_empty() {
        messages.push(task_description.to_string());
    }
    messages.push("Waiting for task (esc to give additional instructions)".to_string());
    messages
}

/// Maps to: CC `tools/SkillTool/UI.tsx:75-135#renderToolUseProgressMessage` —
/// the string projection of the SKILL progress view only.
///
/// The Agent arm re-routed to its tool owner
/// (`tools/agent_tool/ui.rs#AgentToolUseProgressMessage`, CC
/// `AgentTool/UI.tsx:516-721`) — the last Agent renderer to leave this
/// generic owner (#147 item 8's remainder) — and, at K4-G1, stopped being a
/// string projection at all: it mounts components, so it answers on the
/// element half of the dispatch. Skill is still lines. The two CC renderers
/// share a shape but differ in three ways, which is why Skill keeps its own
/// path:
/// 1. **Row filter** — AgentTool keeps ONLY assistant rows
///    (`UI.tsx:129-135`); SkillTool has no filter (SkillTool/UI.tsx:94-96
///    slices the raw list), so tool_result rows render here.
/// 2. **Window axis + hidden count** — SkillTool slices on `verbose`
///    (SkillTool/UI.tsx:94-96) and counts hidden MESSAGES (`:98`); AgentTool
///    slices on `isTranscriptMode` (UI.tsx:610-612) and counts hidden TOOL
///    USES (`:624-635`).
/// 3. **Hidden line** — SkillTool's `+N more tool use(s)` is bare
///    (SkillTool/UI.tsx:127-131); AgentTool appends `<CtrlOToExpand />`
///    (UI.tsx:712-717).
///
/// A FOURTH difference is not ported: CC hands SkillTool's `MessageComponent`
/// `lookups={EMPTY_LOOKUPS}` (SkillTool/UI.tsx:111) while AgentTool passes real
/// `buildSubagentLookups` output (UI.tsx:653-660). With empty lookups
/// `useGetToolFromMessages` returns null and `UserToolResultMessage` renders
/// nothing (UserToolResultMessage.tsx:43-46), so CC's skill progress view shows
/// a BLANK slot for every nested tool_result while still counting it toward the
/// hidden total. Cometix resolves the tool here, so skill tool_result rows show
/// their typed text. Deliberate hold: it is a visible behaviour change on a
/// path the carrier swap does not otherwise touch.
fn skill_progress_auxiliary_messages(
    progress_messages: &[ToolUseProgressMessage],
    verbose: bool,
) -> Vec<String> {
    use crate::types::message::{AssistantContent, RenderableMessageKind, UserContent};

    // CC `SkillTool/UI.tsx:99-101` builds the lookups over the whole progress
    // list before rendering any row.
    let lookups = crate::tools::agent_tool::ui::subagent_progress_lookups(progress_messages);
    let rows = progress_messages
        .iter()
        .filter_map(|progress| {
            // CC `hasProgressMessage` (AgentTool/UI.tsx:61-67, shared guard):
            // payloads from other progress producers carry no message and are
            // skipped.
            let message = progress.subagent_progress_message()?;
            // CC mounts `MessageComponent` per message, which maps every
            // content block; the producer normalized first
            // (SkillTool.ts:250-258 forwards one message), so there is exactly
            // one real block and the first IS all of them.
            match &message.kind {
                RenderableMessageKind::Assistant { message } => {
                    match message.first_content_block()? {
                        AssistantContent::ToolUse(tool_use) => {
                            let tool_name = tool_use.name.trim();
                            // CC mounts the full `MessageComponent` with `tools`
                            // here too (`SkillTool/UI.tsx:108-124`), so this row
                            // runs the WHOLE `AssistantToolUseMessage` gate
                            // chain, not just the owning tool's
                            // `renderToolUseMessage`. Four gates precede the
                            // summary, and this row must reproduce each by hand
                            // because a collapsed row is a String, not a mount:
                            //   `:141-143` empty `userFacingName`
                            //   `:145`     `input.success` (schema)
                            //   `:148-150` a null summary
                            // Every one of them renders NOTHING.
                            //
                            // Rendering nothing is NOT dropping the row: the
                            // message still occupies its slot in the slice
                            // window and the hidden-message count, so only the
                            // STRING goes empty and the drop happens after the
                            // window below.
                            //
                            // The name is the user-facing one. This row used to
                            // interpolate the wire name, so a nested `Grep`
                            // rendered `Grep (…)` where CC renders `Search (…)`
                            // (`GrepTool.ts:161` `name: 'Grep'` vs `:169-171`
                            // `userFacingName() { return 'Search' }`).
                            let display_name =
                                tool_use_display_name(tool_name, Some(&tool_use.input));
                            let row = if !assistant_tool_use_should_render(tool_name)
                                || display_name.is_empty()
                                || !assistant_tool_use_input_parses(
                                    tool_name,
                                    Some(&tool_use.input),
                                ) {
                                String::new()
                            } else {
                                match render_tool_use_message(
                                    tool_name,
                                    &tool_use.input,
                                    ToolRenderOptions {
                                        verbose,
                                        ..Default::default()
                                    },
                                ) {
                                    // CC `:148-150` — a null summary hides the
                                    // row; an EMPTY one (`:186`
                                    // `renderedToolUseMessage !== ''`) keeps the
                                    // name and drops the parentheses.
                                    None => String::new(),
                                    Some(description) => {
                                        let description = description.trim();
                                        if description.is_empty() {
                                            display_name
                                        } else {
                                            format!("{display_name} ({description})")
                                        }
                                    }
                                }
                            };
                            Some(row)
                        }
                        // Same layering as the tool_use arm: the message keeps
                        // its slice-window slot even when the mounted component
                        // renders null (empty text drops after the window).
                        AssistantContent::Text(text) => Some(text.trim().to_string()),
                        _ => None,
                    }
                }
                RenderableMessageKind::User { message } => match message.first_content_block()? {
                    UserContent::ToolResult(result) => {
                        // CC resolves the nested tool through the lookups
                        // (`buildSubagentLookups` → `MessageComponent`), never
                        // off the payload.
                        let tool_use = lookups.tool_use(result.tool_use_id.0.as_str())?;
                        Some(
                            subagent_tool_result_progress_text(
                                tool_use.name.as_str(),
                                if result.is_error {
                                    crate::types::message::ToolResultStatus::Error
                                } else {
                                    crate::types::message::ToolResultStatus::Success
                                },
                                result.content.as_str(),
                                result.tool_use_result.as_ref(),
                                verbose,
                            )
                            .unwrap_or_default(),
                        )
                    }
                    _ => None,
                },
                _ => None,
            }
        })
        .collect::<Vec<_>>();

    // CC `SkillTool/UI.tsx:94-98`: the window axis is `verbose`, and the
    // hidden count is a MESSAGE count — both unlike the Agent renderer.
    let hidden_row_count = if verbose {
        0
    } else {
        rows.len().saturating_sub(3)
    };
    // A gated row (`String::new()` above) is CC's `MessageComponent` returning
    // null: no visible line, but it already counted toward the slice window
    // and the hidden count. So the drop happens HERE, after both, never at
    // construction.
    let mut visible = rows
        .into_iter()
        .skip(hidden_row_count)
        .filter(|row| !row.is_empty())
        .collect::<Vec<String>>();
    if hidden_row_count > 0 {
        // CC SkillTool/UI.tsx:127-131 — `+N more tool use(s)` with NO hint;
        // only the agent renderer appends `<CtrlOToExpand/>`.
        let plural = if hidden_row_count == 1 { "use" } else { "uses" };
        visible.push(format!("+{hidden_row_count} more tool {plural}"));
    }
    visible
}

fn subagent_tool_result_progress_text(
    tool_name: &str,
    status: crate::types::message::ToolResultStatus,
    content: &str,
    tool_use_result: Option<&serde_json::Value>,
    verbose: bool,
) -> Option<String> {
    let tool_name = tool_name.trim();
    if tool_name.is_empty() {
        return None;
    }
    let content = content.trim();
    let lines =
        crate::components::messages::user_tool_result_message::render_tool_result_lines_for_result(
            tool_name,
            status,
            content,
            tool_use_result,
            None,
            &[],
            ToolRenderOptions {
                verbose,
                is_transcript_mode: false,
                ..ToolRenderOptions::default()
            },
        );
    let mut texts = lines
        .into_iter()
        .map(|line| {
            if line.text.is_empty() && !line.segments.is_empty() {
                line.segments
                    .into_iter()
                    .map(|segment| segment.text)
                    .collect::<String>()
            } else {
                line.text
            }
        })
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    if texts.is_empty() {
        // CC renders the nested result through the tool's own
        // `renderToolResultMessage`; when that yields nothing there is no row to
        // show. Only plain text degrades to the raw content, never a payload.
        let is_payload = matches!(
            serde_json::from_str::<serde_json::Value>(content),
            Ok(serde_json::Value::Object(_) | serde_json::Value::Array(_))
        );
        (!content.is_empty() && !is_payload).then(|| content.to_string())
    } else if verbose {
        Some(texts.join("\n"))
    } else {
        Some(texts.remove(0))
    }
}

// `subagent_operation_summary_text` is gone with the `SubagentOperationSummary`
// payload variant. CC's `SummaryMessage` is a type LOCAL to
// `processProgressMessages` (UI.tsx:105-112) produced only on the ant-only
// branch — `if ("external" !== 'ant') { return …original… }` at `:127` returns
// before any grouping runs, so the external build never sees a summary row.
// Rust had no producer either. The summary COPY it duplicated lives on with
// its real CC owner, `utils/collapseReadSearch.ts#getSearchReadSummaryText`
// (`crate::utils::collapse_read_search::get_search_read_summary_text`), which
// `extract_last_tool_info` calls.
//
// `repl_progress_auxiliary_messages` and `workflow_progress_auxiliary_messages`
// are gone: their payload variants were stub inventions — CC has
// no `repl`/`workflow` progress data producer anywhere in the tree (the
// `REPLToolProgress`/`SdkWorkflowProgress` names exist only in the generated
// stub `types/tools.ts` and type-only re-exports; `WorkflowTool.ts` and
// `tools/REPLTool/` emit no progress, and SDK workflow progress travels the
// separate `sdkEventQueue.ts:33 workflow_progress` channel, not
// `ProgressMessage.data`).

/// Maps to: CC `tools/MCPTool/UI.tsx:82-112` — the body of
/// `renderToolUseProgressMessage` once `lastProgress.data` is in hand.
fn mcp_progress_auxiliary_messages(
    progress: Option<f64>,
    total: Option<f64>,
    progress_message: Option<&str>,
) -> Vec<String> {
    // CC `:84-90` — `progress === undefined`.
    let Some(progress) = progress else {
        return vec!["Running…".to_string()];
    };

    if let Some(total) = total.filter(|total| *total > 0.0) {
        let ratio = (progress / total).clamp(0.0, 1.0) as f32;
        let percentage = (ratio * 100.0).round() as u32;
        let mut messages = Vec::new();
        // CC `:98` `{progressMessage && …}` — truthiness, so an empty message
        // renders no line at all above the bar.
        if let Some(message) = progress_message.filter(|message| !message.is_empty()) {
            messages.push(message.to_string());
        }
        messages.push(format!("{} {percentage}%", progress_bar_text(ratio, 20)));
        return messages;
    }

    // CC `:110` `progressMessage ?? \`Processing… ${progress}\`` — NULLISH, the
    // opposite of `:98`. An empty `progressMessage` wins over the fallback and
    // renders as an empty row.
    vec![progress_message.map(ToOwned::to_owned).unwrap_or_else(|| {
        // `${progress}` is `Number::toString`, which is `1` for `1.0` and
        // `1e+21` past the exponent threshold — the zod carrier's funnel.
        format!(
            "Processing… {}",
            crate::utils::zod::javascript_number_to_string(progress)
        )
    })]
}

fn shell_time_display(
    elapsed_time_seconds: Option<u64>,
    timeout_ms: Option<u64>,
) -> Option<String> {
    match (elapsed_time_seconds, timeout_ms) {
        (None, None) => None,
        (None, Some(timeout_ms)) => Some(format!(
            "(timeout {})",
            format_duration_ms(timeout_ms, true)
        )),
        (Some(elapsed_seconds), None) => Some(format!(
            "({})",
            format_duration_ms(elapsed_seconds.saturating_mul(1000), false)
        )),
        (Some(elapsed_seconds), Some(timeout_ms)) => Some(format!(
            "({} · timeout {})",
            format_duration_ms(elapsed_seconds.saturating_mul(1000), false),
            format_duration_ms(timeout_ms, true)
        )),
    }
}

fn format_duration_ms(ms: u64, hide_trailing_zeros: bool) -> String {
    if ms < 60_000 {
        return format!("{}s", ms / 1000);
    }

    let mut days = ms / 86_400_000;
    let mut hours = (ms % 86_400_000) / 3_600_000;
    let mut minutes = (ms % 3_600_000) / 60_000;
    let mut seconds = ((ms % 60_000) + 500) / 1000;

    if seconds == 60 {
        seconds = 0;
        minutes += 1;
    }
    if minutes == 60 {
        minutes = 0;
        hours += 1;
    }
    if hours == 24 {
        hours = 0;
        days += 1;
    }

    if days > 0 {
        if hide_trailing_zeros && hours == 0 && minutes == 0 {
            return format!("{days}d");
        }
        if hide_trailing_zeros && minutes == 0 {
            return format!("{days}d {hours}h");
        }
        return format!("{days}d {hours}h {minutes}m");
    }
    if hours > 0 {
        if hide_trailing_zeros && minutes == 0 && seconds == 0 {
            return format!("{hours}h");
        }
        if hide_trailing_zeros && seconds == 0 {
            return format!("{hours}h {minutes}m");
        }
        return format!("{hours}h {minutes}m {seconds}s");
    }
    if minutes > 0 {
        if hide_trailing_zeros && seconds == 0 {
            return format!("{minutes}m");
        }
        return format!("{minutes}m {seconds}s");
    }
    format!("{seconds}s")
}

fn format_shell_file_size(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb < 1.0 {
        return format!("{bytes} bytes");
    }
    if kb < 1024.0 {
        return format!("{}KB", strip_trailing_dot_zero(kb));
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{}MB", strip_trailing_dot_zero(mb));
    }
    let gb = mb / 1024.0;
    format!("{}GB", strip_trailing_dot_zero(gb))
}

fn strip_trailing_dot_zero(value: f64) -> String {
    let formatted = format!("{value:.1}");
    formatted
        .strip_suffix(".0")
        .unwrap_or(&formatted)
        .to_string()
}

fn join_non_empty(parts: impl Iterator<Item = Option<String>>) -> String {
    parts
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_ansi_sequences(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::figures::BLACK_CIRCLE;
    use crate::types::message::RenderableMessage;

    /// One `agent_progress` payload exactly as the producer emits it: a
    /// normalized single-block message (`AgentTool.tsx:1483-1506`).
    fn agent_progress(message: RenderableMessage) -> ToolUseProgressMessage {
        ToolUseProgressMessage::AgentProgress {
            message: Box::new(message),
            // The loop literal's empty prompt (`AgentTool.tsx:1500-1502`).
            prompt: String::new(),
            agent_id: "agent-1".to_string(),
        }
    }

    fn subagent_tool_use(id: &str, name: &str, input: serde_json::Value) -> ToolUseProgressMessage {
        agent_progress(RenderableMessage::assistant_block(
            format!("assistant-{id}"),
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(id.to_string()),
                name: name.to_string(),
                input,
            }),
        ))
    }

    fn subagent_tool_result(
        id: &str,
        content: &str,
        tool_use_result: Option<serde_json::Value>,
    ) -> ToolUseProgressMessage {
        agent_progress(
            RenderableMessage::user_tool_result(format!("user-{id}"), id, content, false)
                .with_tool_use_result(tool_use_result),
        )
    }

    /// An assistant TEXT row. CC's producer filters these out
    /// (`AgentTool.tsx:1485-1491` forwards only tool_use/tool_result blocks),
    /// but the RENDERER has no block filter, so a text row is what
    /// `MessageComponent` would show if one arrived — the derivation this
    /// exercises.
    fn subagent_text(text: &str) -> ToolUseProgressMessage {
        agent_progress(RenderableMessage::assistant_block(
            format!("assistant-text-{text}"),
            crate::types::message::AssistantContent::Text(text.to_string()),
        ))
    }

    /// The collapsed in-flight Agent view with the replay re-entry's defaults —
    /// no terminal size, no in-progress count (CC `UI.tsx:755-759` passes
    /// neither), main screen. The Agent arm lives with its tool owner since the
    /// #154 merge; the side-by-side Skill comparisons below keep calling it
    /// from here.
    ///
    /// It RENDERS now: since the K4-G1 mount the Agent renderer is a component
    /// (CC's `renderToolUseProgressMessage` returns a React node), so these
    /// rows come off a canvas with the `MessageResponse` gutter stripped rather
    /// than out of a `Vec<String>`. The Skill half below is still a string
    /// projection, which is why only this side needs the strip.
    fn agent_auxiliary_lines(progress: &[ToolUseProgressMessage], verbose: bool) -> Vec<String> {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                crate::tools::agent_tool::ui::AgentToolUseProgressMessage(
                    progress_messages: progress.to_vec(),
                    verbose: verbose,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(80));
        canvas
            .to_string()
            .lines()
            .map(|line| {
                let line = line.trim_end();
                line.strip_prefix("  ⎿ ")
                    .or_else(|| line.strip_prefix("  ⎿"))
                    .or_else(|| line.strip_prefix("    "))
                    .unwrap_or(line)
                    .to_string()
            })
            .collect()
    }

    fn render_tool_header_canvas(
        tool_name: &str,
        input: serde_json::Value,
        verbose: bool,
        width: usize,
    ) -> Canvas {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: tool_name.to_string(),
                    input: Some(input),
                    status: Some(ToolUseStatus::Succeeded),
                    can_animate: false,
                    verbose: verbose,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(width))
    }

    fn hyperlink_text(canvas: &Canvas, url: &str) -> String {
        let mut text = String::new();
        for row in 0..canvas.height() {
            for col in 0..canvas.width() {
                let Some(cell) = canvas.cell(col, row) else {
                    continue;
                };
                if cell.hyperlink() == Some(url) {
                    if let Some(value) = cell.text() {
                        text.push_str(value);
                    }
                }
            }
        }
        text
    }

    /// Maps to: CC `AssistantToolUseMessage.tsx:120-121`.
    #[test]
    fn tool_use_status_is_derived_from_the_two_sets() {
        use crate::components::messages_list::MessageLookups;
        let live: std::collections::HashSet<String> = ["toolu_1".to_string()].into_iter().collect();
        let idle = std::collections::HashSet::new();

        let mut resolved = MessageLookups::default();
        resolved.resolved_tool_use_ids.insert("toolu_1".to_string());
        let mut errored = resolved.clone();
        errored.errored_tool_use_ids.insert("toolu_1".to_string());

        // Not started and not resolved → queued (CC's `isQueued`).
        assert_eq!(
            derive_tool_use_status(Some("toolu_1"), &idle, None),
            ToolUseStatus::Queued
        );
        assert_eq!(
            derive_tool_use_status(Some("toolu_1"), &live, None),
            ToolUseStatus::Running
        );
        // `isResolved` wins over set membership: a result having arrived is
        // terminal even if the actor has not yet sent the set removal.
        assert_eq!(
            derive_tool_use_status(Some("toolu_1"), &live, Some(&resolved)),
            ToolUseStatus::Succeeded
        );
        assert_eq!(
            derive_tool_use_status(Some("toolu_1"), &idle, Some(&errored)),
            ToolUseStatus::Failed
        );
        // A row with no id can never be matched to a result.
        assert_eq!(
            derive_tool_use_status(None, &live, Some(&resolved)),
            ToolUseStatus::Queued
        );
    }

    /// Maps to: CC `FileEditTool/UI.tsx:82-86`,
    /// `FileWriteTool/UI.tsx:130-135`, `NotebookEditTool/UI.tsx:41-53` —
    /// the header path renders inside `FilePathLink`, whose visible label is
    /// the cwd-relative display path (absolute only when verbose). The
    /// NotebookEdit `@cell_id` remainder stays outside the link.
    #[test]
    fn file_tool_headers_link_the_path_with_the_display_label() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let cwd = crate::bootstrap::state::get_original_cwd();
        let absolute = cwd.join("nb_dir/demo.rs").display().to_string();
        let render = |tool_name: &str, input: serde_json::Value, verbose: bool| {
            element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    AssistantToolUseMessage(
                        tool_name: tool_name.to_string(),
                        input: Some(input),
                        status: Some(ToolUseStatus::Succeeded),
                        can_animate: false,
                        verbose: verbose,
                        is_transcript_mode: false,
                    )
                }
            }
            .render(Some(200))
            .to_string()
        };

        let edit = render(
            "Edit",
            serde_json::json!({
                "file_path": absolute, "old_string": "a", "new_string": "b"
            }),
            false,
        );
        assert!(edit.contains("nb_dir/demo.rs"), "canvas=\n{edit}");
        assert!(
            !edit.contains(cwd.display().to_string().as_str()),
            "canvas=\n{edit}"
        );

        let write = render(
            "Write",
            serde_json::json!({"file_path": absolute, "content": "x"}),
            false,
        );
        assert!(write.contains("nb_dir/demo.rs"), "canvas=\n{write}");
        assert!(
            !write.contains(cwd.display().to_string().as_str()),
            "canvas=\n{write}"
        );

        // NotebookEdit keeps `@cell_id` after the linked path.
        let notebook_path = cwd.join("nb_dir/demo.ipynb").display().to_string();
        let notebook = render(
            "NotebookEdit",
            serde_json::json!({
                "notebook_path": notebook_path,
                "cell_id": "cell-a",
                "new_source": "print('x')",
                "cell_type": "code",
                "edit_mode": "insert"
            }),
            false,
        );
        assert!(
            notebook.contains("nb_dir/demo.ipynb@cell-a"),
            "canvas=\n{notebook}"
        );

        // Verbose keeps the absolute path as the visible label.
        let verbose_edit = render(
            "Edit",
            serde_json::json!({
                "file_path": absolute, "old_string": "a", "new_string": "b"
            }),
            true,
        );
        assert!(
            verbose_edit.contains(cwd.display().to_string().as_str()),
            "canvas=\n{verbose_edit}"
        );
    }

    /// Maps to CC `AssistantToolUseMessage.tsx:193-196` and
    /// `NotebookEditTool/UI.tsx:41-53`: punctuation, link, and suffix are one
    /// wrapping Text flow while only the path carries OSC-8 metadata.
    #[test]
    fn notebook_header_wraps_as_one_linked_text_flow_like_official() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        // CC Link.tsx only attaches OSC-8 metadata on supported terminals.
        let _terminal = crate::utils::env_utils::EnvVarGuard::set("TERM_PROGRAM", "kitty");
        let cwd = crate::bootstrap::state::get_original_cwd();
        let notebook_path = cwd.join("nb_dir/demo.ipynb").display().to_string();
        let canvas = render_tool_header_canvas(
            "NotebookEdit",
            serde_json::json!({
                "notebook_path": notebook_path,
                "cell_id": "cell-a",
                "new_source": "print('a deliberately long notebook source')",
                "cell_type": "code",
                "edit_mode": "insert"
            }),
            false,
            24,
        );

        assert_eq!(
            canvas.to_string(),
            "⏺ Edit Notebook(nb_dir/\n               demo.ipyn\n               b@cell-a)\n"
        );
        let url = crate::components::file_path_link::file_path_to_file_url(&notebook_path);
        assert_eq!(hyperlink_text(&canvas, &url), "nb_dir/demo.ipynb");
        assert!(canvas.soft_wrap_continuation(1) > 0);
        assert!(canvas.soft_wrap_continuation(2) > 0);
    }

    /// Maps to CC `FileReadTool/UI.tsx:35-71`, `FileEditTool/UI.tsx:71-87`,
    /// `FileWriteTool/UI.tsx:120-136`, and `NotebookEditTool/UI.tsx:26-54`.
    #[test]
    fn scoped_file_headers_keep_exact_copy_and_path_only_links_like_official() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        // CC Link.tsx only attaches OSC-8 metadata on supported terminals.
        let _terminal = crate::utils::env_utils::EnvVarGuard::set("TERM_PROGRAM", "kitty");
        let cwd = crate::bootstrap::state::get_original_cwd();

        let read_path = cwd.join("manual.pdf").display().to_string();
        let read = render_tool_header_canvas(
            "Read",
            serde_json::json!({"file_path": read_path, "pages": "2-4"}),
            false,
            24,
        );
        assert_eq!(read.to_string(), "⏺ Read(manual.pdf ·\n      pages 2-4)\n");
        let read_url = crate::components::file_path_link::file_path_to_file_url(&read_path);
        assert_eq!(hyperlink_text(&read, &read_url), "manual.pdf");

        let read_plain = render_tool_header_canvas(
            "Read",
            serde_json::json!({"file_path": read_path}),
            false,
            80,
        );
        assert_eq!(read_plain.to_string(), "⏺ Read(manual.pdf)\n");
        assert_eq!(hyperlink_text(&read_plain, &read_url), "manual.pdf");

        let range_path = cwd.join("manual.txt").display().to_string();
        let read_range = render_tool_header_canvas(
            "Read",
            serde_json::json!({"file_path": range_path, "offset": 10, "limit": 3}),
            true,
            200,
        );
        assert_eq!(
            read_range.to_string(),
            format!("⏺ Read({range_path} · lines 10-12)\n")
        );
        let range_url = crate::components::file_path_link::file_path_to_file_url(&range_path);
        assert_eq!(hyperlink_text(&read_range, &range_url), range_path);

        let read_from = render_tool_header_canvas(
            "Read",
            serde_json::json!({"file_path": range_path, "offset": 10}),
            true,
            200,
        );
        assert_eq!(
            read_from.to_string(),
            format!("⏺ Read({range_path} · from line 10)\n")
        );
        assert_eq!(hyperlink_text(&read_from, &range_url), range_path);

        let edit_path = cwd.join("src/main.rs").display().to_string();
        let edit = render_tool_header_canvas(
            "Edit",
            serde_json::json!({
                "file_path": edit_path,
                "old_string": "a",
                "new_string": "b"
            }),
            false,
            24,
        );
        assert_eq!(edit.to_string(), "⏺ Update(src/main.rs)\n");
        let edit_url = crate::components::file_path_link::file_path_to_file_url(&edit_path);
        assert_eq!(hyperlink_text(&edit, &edit_url), "src/main.rs");

        let narrow_edit_path = cwd.join("src/very_long_name.rs").display().to_string();
        let narrow_edit = render_tool_header_canvas(
            "Edit",
            serde_json::json!({
                "file_path": narrow_edit_path,
                "old_string": "a",
                "new_string": "b"
            }),
            false,
            18,
        );
        assert_eq!(
            narrow_edit.to_string(),
            "⏺ Update(src/\n        very_long_\n        name.rs)\n"
        );
        let narrow_edit_url =
            crate::components::file_path_link::file_path_to_file_url(&narrow_edit_path);
        assert_eq!(
            hyperlink_text(&narrow_edit, &narrow_edit_url),
            "src/very_long_name.rs"
        );

        let write_path = cwd.join("src/output.rs").display().to_string();
        let write = render_tool_header_canvas(
            "Write",
            serde_json::json!({"file_path": write_path, "content": "x"}),
            false,
            24,
        );
        assert_eq!(write.to_string(), "⏺ Write(src/output.rs)\n");
        let write_url = crate::components::file_path_link::file_path_to_file_url(&write_path);
        assert_eq!(hyperlink_text(&write, &write_url), "src/output.rs");

        let narrow_write_path = cwd.join("src/very_long_output.rs").display().to_string();
        let narrow_write = render_tool_header_canvas(
            "Write",
            serde_json::json!({"file_path": narrow_write_path, "content": "x"}),
            false,
            18,
        );
        assert_eq!(
            narrow_write.to_string(),
            "⏺ Write(src/\n       very_long_o\n       utput.rs)\n"
        );
        let narrow_write_url =
            crate::components::file_path_link::file_path_to_file_url(&narrow_write_path);
        assert_eq!(
            hyperlink_text(&narrow_write, &narrow_write_url),
            "src/very_long_output.rs"
        );

        let notebook_path = cwd.join("nb_dir/demo.ipynb").display().to_string();
        let verbose = render_tool_header_canvas(
            "NotebookEdit",
            serde_json::json!({
                "notebook_path": notebook_path,
                "cell_id": "cell-a",
                "new_source": "print('a deliberately long notebook source')",
                "cell_type": "code",
                "edit_mode": "insert"
            }),
            true,
            48,
        );
        let verbose_text = verbose.to_string();
        assert!(verbose_text.contains("@cell-a,"), "canvas=\n{verbose_text}");
        assert!(
            verbose_text.contains("content: print('a deliberately"),
            "canvas=\n{verbose_text}"
        );
        assert!(
            verbose_text.contains("long not…, cell_type: code,"),
            "canvas=\n{verbose_text}"
        );
        assert!(
            verbose_text.contains("edit_mode: insert)"),
            "canvas=\n{verbose_text}"
        );
        let notebook_url = crate::components::file_path_link::file_path_to_file_url(&notebook_path);
        assert_eq!(hyperlink_text(&verbose, &notebook_url), notebook_path);

        let verbose_wide = render_tool_header_canvas(
            "NotebookEdit",
            serde_json::json!({
                "notebook_path": notebook_path,
                "cell_id": "cell-a",
                "new_source": "print('a deliberately long notebook source')",
                "cell_type": "code",
                "edit_mode": "insert"
            }),
            true,
            256,
        );
        assert_eq!(
            verbose_wide.to_string(),
            format!(
                "⏺ Edit Notebook({notebook_path}@cell-a, content: print('a deliberately long not…, cell_type: code, edit_mode: insert)\n"
            )
        );
        assert_eq!(hyperlink_text(&verbose_wide, &notebook_url), notebook_path);

        let defaults = render_tool_header_canvas(
            "NotebookEdit",
            serde_json::json!({
                "notebook_path": notebook_path,
                "new_source": "print('short')",
                "cell_type": "code"
            }),
            true,
            256,
        );
        assert_eq!(
            defaults.to_string(),
            format!(
                "⏺ Edit Notebook({notebook_path}@undefined, content: print('short')…, cell_type: code, edit_mode: replace)\n"
            )
        );
        assert_eq!(hyperlink_text(&defaults, &notebook_url), notebook_path);

        let plan_path = crate::utils::plans::get_plans_directory()
            .join("plan.md")
            .display()
            .to_string();
        let plan_url = crate::components::file_path_link::file_path_to_file_url(&plan_path);
        for (tool_name, input) in [
            (
                "Edit",
                serde_json::json!({
                    "file_path": plan_path,
                    "old_string": "a",
                    "new_string": "b"
                }),
            ),
            (
                "Write",
                serde_json::json!({"file_path": plan_path, "content": "plan"}),
            ),
        ] {
            let plan = render_tool_header_canvas(tool_name, input, false, 80);
            assert_eq!(plan.to_string(), "⏺ Updated plan\n");
            assert_eq!(hyperlink_text(&plan, &plan_url), "");
        }
    }

    fn auxiliary_message(
        tool_name: &str,
        status: ToolUseStatus,
        is_classifier_checking: bool,
        is_auto_classifier: bool,
        is_waiting_for_permission: bool,
    ) -> Option<String> {
        tool_use_auxiliary_message(
            tool_name,
            status,
            is_classifier_checking,
            is_auto_classifier,
            is_waiting_for_permission,
            &[],
        )
    }

    fn some_message(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    fn canvas_lines(canvas: &Canvas) -> Vec<String> {
        (0..canvas.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..canvas.width() {
                    if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text()) {
                        line.push_str(text);
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn find_text_cell(canvas: &Canvas, needle: &str) -> Option<(usize, usize)> {
        canvas_lines(canvas)
            .iter()
            .enumerate()
            .find_map(|(row, line)| line.find(needle).map(|column| (column, row)))
    }

    #[test]
    fn notebook_tool_use_hides_null_renderer_and_never_leaks_raw_json() {
        let path = crate::bootstrap::state::get_original_cwd().join("notebooks/demo.ipynb");
        let replace = serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-a",
            "new_source": "new"
        });
        assert!(assistant_tool_use_is_nonvisual(
            "NotebookEdit",
            Some(&replace)
        ));
        let mut hidden = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "NotebookEdit".to_string(),
                    input: Some(replace.clone()),
                    status: Some(ToolUseStatus::Succeeded),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        };
        assert!(hidden.render(Some(120)).to_string().trim().is_empty());

        let insert = serde_json::json!({
            "notebook_path": path.display().to_string(),
            "cell_id": "cell-a",
            "new_source": "# inserted",
            "cell_type": "markdown",
            "edit_mode": "insert"
        });
        assert!(!assistant_tool_use_is_nonvisual(
            "NotebookEdit",
            Some(&insert)
        ));
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "NotebookEdit".to_string(),
                    input: Some(insert.clone()),
                    status: Some(ToolUseStatus::Succeeded),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Edit Notebook"), "canvas=\n{text}");
        assert!(text.contains("demo.ipynb@cell-a"), "canvas=\n{text}");
        assert!(!text.contains("notebook_path"), "canvas=\n{text}");
        assert!(!text.contains("new_source"), "canvas=\n{text}");
    }

    #[test]
    fn live_grep_row_uses_official_search_facing_name() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Grep".to_string(),
                    input: Some(serde_json::json!({"pattern": "needle"})),
                    status: Some(ToolUseStatus::Succeeded),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Search"), "canvas=\n{text}");
        assert!(text.contains("pattern: \"needle\""), "canvas=\n{text}");
        assert!(!text.contains("Grep"), "canvas=\n{text}");
    }

    /// CC `AgentTool/UI.tsx:472-483`: `if (!description || !prompt) return
    /// null` — JS-truthy on BOTH fields; the row is hidden rather than
    /// degrading to a bare "Agent" through the shared fallback.
    #[test]
    fn agent_rows_hide_when_description_or_prompt_is_missing_or_empty() {
        let render = |input: serde_json::Value| {
            render_tool_use_message("Agent", &input, ToolRenderOptions::default())
        };
        assert_eq!(
            render(serde_json::json!({ "description": "Review", "prompt": "check it" })),
            Some("Review".to_string())
        );
        assert_eq!(render(serde_json::json!({ "prompt": "check it" })), None);
        assert_eq!(render(serde_json::json!({ "description": "Review" })), None);
        assert_eq!(
            render(serde_json::json!({ "description": "", "prompt": "check it" })),
            None
        );
        assert_eq!(
            render(serde_json::json!({ "description": "Review", "prompt": "" })),
            None
        );
    }

    /// CC `AssistantToolUseMessage.tsx:88` parses the row's input ONCE and
    /// `:145-150` hides the whole row when that parse failed — the tool's own
    /// renderer never runs.
    ///
    /// The Agent case is audit follow-up 11's cited example: `description` and
    /// `prompt` are both truthy, so `AgentTool/UI.tsx:472-483` would happily
    /// return "Review", but `model` is `z.enum(['sonnet','opus','haiku'])`
    /// (`AgentTool.tsx:240-255`) and `"gpt"` fails it. Before the gate that row
    /// rendered here and was hidden in CC.
    #[test]
    fn rows_hide_when_their_input_schema_rejects_the_input() {
        let render = |tool_name: &str, input: serde_json::Value| {
            element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    AssistantToolUseMessage(
                        tool_name: tool_name.to_string(),
                        input: Some(input),
                        status: Some(ToolUseStatus::Succeeded),
                        can_animate: false,
                        verbose: false,
                        is_transcript_mode: false,
                    )
                }
            }
            .render(Some(120))
            .to_string()
        };

        // Positive control: the same row without the bad member still renders,
        // so the assertion below cannot pass by hiding everything.
        let good = render(
            "Agent",
            serde_json::json!({ "description": "Review", "prompt": "check it" }),
        );
        assert!(good.contains("Review"), "canvas=\n{good}");

        let bad_enum = render(
            "Agent",
            serde_json::json!({
                "description": "Review", "prompt": "check it", "model": "gpt"
            }),
        );
        assert_eq!(bad_enum.trim(), "", "canvas=\n{bad_enum}");

        // Not Agent-specific: `BashTool.inputSchema` is a `z.strictObject`
        // (`BashTool.tsx:337-431`), so a wrong-typed `command` is
        // `invalid_type`. Without the gate this degraded to a bare "Bash".
        let bash_ok = render("Bash", serde_json::json!({ "command": "ls" }));
        assert!(bash_ok.contains("ls"), "canvas=\n{bash_ok}");
        let bash_bad = render("Bash", serde_json::json!({ "command": 42 }));
        assert_eq!(bash_bad.trim(), "", "canvas=\n{bash_bad}");
    }

    /// The seam's boundary, stated as a test so it cannot erode: the gate fires
    /// only for names CC's `findToolByName` (`Tool.ts:348-360`) would resolve to
    /// a tool — its `name` or one of its `aliases`. Everything else keeps
    /// rendering, because "this port has no schema filed under that name" is not
    /// "safeParse failed".
    #[test]
    fn the_input_schema_gate_covers_only_names_cc_resolves_to_a_tool() {
        let parses = |tool_name: &str, input: serde_json::Value| {
            assistant_tool_use_input_parses(tool_name, Some(&input))
        };

        // Gated, and the gate has teeth.
        assert!(parses("Read", serde_json::json!({ "file_path": "/a" })));
        assert!(!parses("Read", serde_json::json!({})));
        assert!(!parses(
            "Agent",
            serde_json::json!({ "description": "d", "prompt": "p", "model": "gpt" })
        ));
        // CC's three alias declarations resolve to the aliased tool's schema:
        // `AgentTool.tsx:374` → 'Task', `TaskStopTool.ts:44` → 'KillShell'
        // (whose schema is the one that still declares the deprecated
        // `shell_id`, `TaskStopTool.ts:10-19`).
        assert!(!parses(
            "Task",
            serde_json::json!({ "description": "d", "prompt": "p", "model": "gpt" })
        ));
        assert!(parses("KillShell", serde_json::json!({ "shell_id": "x" })));

        // MCP rows are the population a literal port would delete: CC's pool
        // carries them (`REPL.tsx:3176-3179`) so `findToolByName` resolves the
        // per-server tool, which Rust has no schema for at this boundary.
        assert!(parses(
            "mcp__github__create_issue",
            serde_json::json!({ "title": "t", "anything": [1, 2] })
        ));
        // Aliases this port invented for rows written by other versions. CC has
        // no tool of these names at all, so no CC schema applies to them.
        assert!(parses("BashOutput", serde_json::json!({ "bash_id": "x" })));
        assert!(parses(
            "MultiEdit",
            serde_json::json!({ "file_path": "/a", "edits": [] })
        ));
        assert!(parses("ListMcpResources", serde_json::json!({})));
        // An unknown tool renders through the `_` fallback arm, ungated.
        assert!(parses("SomeFutureTool", serde_json::json!({ "x": 1 })));

        // A row recovered without a `ToolUseBlock` has no input to parse and
        // renders its pre-baked `description` instead.
        assert!(assistant_tool_use_input_parses("Read", None));
    }

    /// The gate's FALSE-POSITIVE face, which is the dangerous one: a row the
    /// user should see must never disappear. CC only hides a row when
    /// `tool.inputSchema.safeParse(param.input)` fails
    /// (`AssistantToolUseMessage.tsx:88`, `:145-150`), so every ordinary
    /// invocation of every gated name has to survive.
    ///
    /// One entry per key in `assistant_tool_use_input_schema`, plus the three
    /// CC alias spellings that resolve into the same schemas. The inputs are
    /// the shapes the model actually emits, not minimal ones, so a schema that
    /// drifted stricter than CC's — an optional field turned required, a lost
    /// `semanticBoolean` preprocessor, a union arm that stopped matching —
    /// fails here instead of silently deleting transcript rows.
    #[test]
    fn every_gated_name_keeps_its_ordinary_rows() {
        let cases: Vec<(&str, serde_json::Value)> = vec![
            ("Read", serde_json::json!({ "file_path": "/a/b.rs" })),
            (
                "Read",
                serde_json::json!({ "file_path": "/a/b.rs", "offset": 10, "limit": 40 }),
            ),
            (
                "Bash",
                serde_json::json!({ "command": "ls -la", "description": "List files" }),
            ),
            (
                "Bash",
                serde_json::json!({ "command": "sleep 1", "timeout": 5000 }),
            ),
            (
                "PowerShell",
                serde_json::json!({ "command": "Get-ChildItem", "description": "List files" }),
            ),
            (
                "Edit",
                serde_json::json!({
                    "file_path": "/a/b.rs", "old_string": "x", "new_string": "y"
                }),
            ),
            // `semanticBoolean` on `replace_all` (`FileEditTool/types.ts`): the
            // string literal is coerced before validation, so it must parse.
            (
                "Edit",
                serde_json::json!({
                    "file_path": "/a/b.rs", "old_string": "x", "new_string": "y",
                    "replace_all": "true"
                }),
            ),
            (
                "Write",
                serde_json::json!({ "file_path": "/a/b.rs", "content": "hello" }),
            ),
            (
                "NotebookEdit",
                serde_json::json!({
                    "notebook_path": "/a/n.ipynb", "new_source": "print(1)",
                    "cell_id": "c1", "cell_type": "code", "edit_mode": "replace"
                }),
            ),
            (
                "Grep",
                serde_json::json!({ "pattern": "needle", "path": "/a", "output_mode": "content" }),
            ),
            (
                "Glob",
                serde_json::json!({ "pattern": "**/*.rs", "path": "/a" }),
            ),
            (
                "WebFetch",
                serde_json::json!({ "url": "https://example.com/x", "prompt": "Summarize" }),
            ),
            (
                "WebSearch",
                serde_json::json!({ "query": "rust zod port", "allowed_domains": ["docs.rs"] }),
            ),
            (
                "Agent",
                serde_json::json!({
                    "description": "Review code", "prompt": "check it",
                    "subagent_type": "general-purpose", "model": "sonnet"
                }),
            ),
            (
                "Task",
                serde_json::json!({ "description": "Review code", "prompt": "check it" }),
            ),
            ("Skill", serde_json::json!({ "skill": "commit" })),
            (
                "Skill",
                serde_json::json!({ "skill": "review-pr", "args": "--fast" }),
            ),
            (
                "SendMessage",
                serde_json::json!({ "to": "alice", "summary": "status", "message": "hello" }),
            ),
            // The `z.union([z.string(), StructuredMessage()])` arm
            // (`SendMessageTool.ts:82-85`).
            (
                "SendMessage",
                serde_json::json!({
                    "to": "*",
                    "message": { "type": "shutdown_request", "reason": "done" }
                }),
            ),
            ("Config", serde_json::json!({ "setting": "theme" })),
            (
                "Config",
                serde_json::json!({ "setting": "theme", "value": "dark" }),
            ),
            // `z.object({}).passthrough()` (`SyntheticOutputTool.ts:11`) — the
            // caller's JSON Schema governs execution, never this gate.
            (
                "StructuredOutput",
                serde_json::json!({ "anything": [1, 2], "nested": { "a": true } }),
            ),
            (
                "TaskOutput",
                serde_json::json!({ "task_id": "task-7", "block": false, "timeout": 1000 }),
            ),
            // `semanticBoolean(z.boolean().default(true))` (`:32-34`).
            (
                "TaskOutput",
                serde_json::json!({ "task_id": "task-7", "block": "false" }),
            ),
            (
                "AgentOutputTool",
                serde_json::json!({ "task_id": "task-7" }),
            ),
            ("BashOutputTool", serde_json::json!({ "task_id": "task-7" })),
            ("TaskStop", serde_json::json!({ "task_id": "task-7" })),
            ("KillShell", serde_json::json!({ "shell_id": "bash-1" })),
            ("EnterWorktree", serde_json::json!({})),
            ("EnterWorktree", serde_json::json!({ "name": "feature/x" })),
            ("ExitWorktree", serde_json::json!({ "action": "keep" })),
            (
                "ExitWorktree",
                serde_json::json!({ "action": "remove", "discard_changes": true }),
            ),
            (
                "LSP",
                serde_json::json!({
                    "operation": "goToDefinition", "filePath": "src/main.rs",
                    "line": 12, "character": 4
                }),
            ),
            ("RemoteTrigger", serde_json::json!({ "action": "list" })),
            (
                "RemoteTrigger",
                serde_json::json!({
                    "action": "create", "trigger_id": "nightly-build",
                    "body": { "branch": "main" }
                }),
            ),
            (
                "CronCreate",
                serde_json::json!({
                    "cron": "*/5 * * * *", "prompt": "check the queue",
                    "recurring": "false"
                }),
            ),
            ("CronDelete", serde_json::json!({ "id": "job-1" })),
            ("CronList", serde_json::json!({})),
            ("ListMcpResourcesTool", serde_json::json!({})),
            (
                "ListMcpResourcesTool",
                serde_json::json!({ "server": "memory" }),
            ),
            (
                "ReadMcpResourceTool",
                serde_json::json!({ "server": "memory", "uri": "mem://note/1" }),
            ),
        ];

        for (tool_name, input) in &cases {
            assert!(
                assistant_tool_use_input_parses(tool_name, Some(input)),
                "{tool_name} must keep rendering for {input}"
            );
        }

        // Coverage guard: every key the gate knows about appears above, so a
        // newly gated tool cannot ship without an ordinary-row case. The names
        // are compared lowercased because that is the dispatch key.
        for name in [
            "read",
            "bash",
            "powershell",
            "edit",
            "write",
            "notebookedit",
            "grep",
            "glob",
            "webfetch",
            "websearch",
            "agent",
            "task",
            "skill",
            "sendmessage",
            "config",
            "structuredoutput",
            "taskoutput",
            "agentoutputtool",
            "bashoutputtool",
            "taskstop",
            "killshell",
            "enterworktree",
            "exitworktree",
            "lsp",
            "remotetrigger",
            "croncreate",
            "crondelete",
            "cronlist",
            "listmcpresourcestool",
            "readmcpresourcetool",
        ] {
            assert!(
                assistant_tool_use_input_schema(name).is_some(),
                "{name} is expected to be gated"
            );
            assert!(
                cases
                    .iter()
                    .any(|(tool_name, _)| tool_name.to_ascii_lowercase() == name),
                "{name} is gated but has no ordinary-row case above"
            );
        }
    }

    /// Composition order. CC checks `isTransparentWrapper` at `:124-139`, BEFORE
    /// the `:141` empty-facing-name return and the `:145-150` parse gate, so a
    /// wrapper row still shows the wrapped tool's progress on an input its
    /// schema rejects.
    #[test]
    fn the_input_schema_gate_runs_after_the_transparent_wrapper_branch() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    // Fails `BashTool.inputSchema`: `command` is required.
                    input: Some(serde_json::json!({})),
                    status: Some(ToolUseStatus::Running),
                    progress_messages: vec![ToolUseProgressMessage::BashProgress {
                        output: "VM line one".to_string(),
                        full_output: "VM line one".to_string(),
                        elapsed_time_seconds: 3,
                        total_lines: 1,
                        total_bytes: None,
                        task_id: None,
                        timeout_ms: None,
                    }],
                    is_transparent_wrapper: true,
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(text.contains("VM line one"), "canvas=\n{text}");

        // The same input on a NON-wrapper row is past the branch, so the gate
        // applies — the pair is what pins the order rather than the outcome.
        let gated = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    input: Some(serde_json::json!({})),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert_eq!(gated.trim(), "", "canvas=\n{gated}");
    }

    /// G12 wiring — `props.is_transcript_mode` must reach the auxiliary
    /// chain: CC threads `isTranscriptMode` from the component
    /// (`AssistantToolUseMessage.tsx:218-230`) into
    /// `renderToolUseProgressMessage`, whose display window slices on it
    /// (`AgentTool/UI.tsx:610-612`). Before the #154 merge the prop existed
    /// on the component but never reached this chain.
    #[test]
    fn agent_collapsed_transcript_mode_widens_the_window_through_the_component() {
        let progress = || {
            (0..5)
                .map(|index| {
                    subagent_tool_use(
                        &format!("toolu_nested_{index}"),
                        "Grep",
                        serde_json::json!({ "pattern": format!("pattern-{index}") }),
                    )
                })
                .collect::<Vec<_>>()
        };
        let render = |is_transcript_mode: bool| {
            element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    AssistantToolUseMessage(
                        tool_name: "Agent".to_string(),
                        description: "investigate parity".to_string(),
                        status: Some(ToolUseStatus::Running),
                        can_animate: false,
                        progress_messages: progress(),
                        verbose: false,
                        is_transcript_mode: is_transcript_mode,
                    )
                }
            }
            .render(None)
            .to_string()
        };

        // The non-transcript half of this wiring test is environment
        // sensitive: crossterm's `terminal::size()` reads /dev/tty before
        // stdout, so a test run from a REAL terminal shorter than 16 rows
        // (count-less estimate 1*9+7, CC UI.tsx:540-549) takes the condensed
        // branch and hides the window rows. The 3-row window itself is pinned
        // terminal-independently by the direct-call test
        // `the_window_axis_is_transcript_mode_not_verbose`; here we accept
        // either shape and only require the main-screen slice signal when the
        // window rendered at all.
        let main = render(false);
        assert!(!main.contains("pattern-0"), "canvas=\n{main}");
        if main.contains("In progress…") {
            assert!(!main.contains("pattern-4"), "canvas=\n{main}");
        } else {
            assert!(main.contains("pattern-4"), "canvas=\n{main}");
            assert!(
                main.contains("+2 more tool uses (ctrl+o to expand)"),
                "canvas=\n{main}"
            );
        }

        // The transcript half is immune: `!isTranscriptMode` is the condensed
        // guard's first condition, so the full list must render.
        let transcript = render(true);
        assert!(transcript.contains("pattern-0"), "canvas=\n{transcript}");
        assert!(transcript.contains("pattern-4"), "canvas=\n{transcript}");
        assert!(!transcript.contains("more tool"), "canvas=\n{transcript}");
    }

    /// G12 all-gated edge, through the component: every visible row renders
    /// null, and CC keeps the outer `MessageResponse` — a bare `⎿` gutter row
    /// (`AgentTool/UI.tsx:664-719`, `:687-691`) — where the old auxiliary
    /// path invented `Initializing…`.
    #[test]
    fn agent_all_gated_progress_renders_a_bare_gutter_not_initializing() {
        let progress = (0..2)
            .map(|index| {
                subagent_tool_use(
                    &format!("toolu_gated_{index}"),
                    "ToolSearch",
                    serde_json::json!({ "query": "bash" }),
                )
            })
            .collect::<Vec<_>>();
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Agent".to_string(),
                    description: "investigate parity".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("⎿"), "canvas=\n{text}");
        assert!(!text.contains("Initializing…"), "canvas=\n{text}");
    }

    /// CC `AgentTool/UI.tsx:129-135` keeps only assistant rows for the agent
    /// progress view (`m.data.message.type !== 'user'`), while
    /// `SkillTool/UI.tsx:94-96` slices the raw list with no such filter.
    #[test]
    fn agent_progress_drops_tool_result_rows_while_skill_keeps_them() {
        let progress = vec![
            subagent_tool_use(
                "toolu_nested_read",
                "Read",
                serde_json::json!({ "file_path": "src/main.rs" }),
            ),
            subagent_tool_result("toolu_nested_read", "read 40 lines", None),
        ];
        let agent_rows = agent_auxiliary_lines(&progress, false);
        assert!(
            agent_rows.iter().all(|row| !row.contains("read 40 lines")),
            "agent rows kept a tool_result row: {agent_rows:?}"
        );
        // The filter must drop ONLY the user rows: CC keeps every assistant row
        // (`m.data.message.type !== 'user'` is the whole predicate), so a
        // regression that emptied the list would otherwise pass the check above.
        //
        // `Read(src/main.rs)`, not `Read (src/main.rs)`: the mounted
        // `AssistantToolUseMessage` puts the bold name and the `(summary)` in
        // two adjacent boxes with nothing between them
        // (`AssistantToolUseMessage.tsx:180-194`). The retired string
        // projection joined them with a space CC never emits.
        assert!(
            agent_rows
                .iter()
                .any(|row| row.contains("Read(src/main.rs)")),
            "agent rows dropped the assistant tool_use row: {agent_rows:?}"
        );
        let skill_rows = skill_progress_auxiliary_messages(&progress, false);
        assert!(
            skill_rows.iter().any(|row| row.contains("read 40 lines")),
            "skill rows dropped a tool_result row: {skill_rows:?}"
        );
    }

    /// The renderers' third difference: CC's agent hidden line carries
    /// `<CtrlOToExpand />` (AgentTool/UI.tsx:712-717); the skill one is bare
    /// (SkillTool/UI.tsx:127-131).
    #[test]
    fn only_the_agent_hidden_line_carries_the_ctrl_o_hint() {
        let progress = (0..5)
            .map(|index| {
                subagent_tool_use(
                    &format!("toolu_nested_{index}"),
                    "Read",
                    serde_json::json!({ "file_path": format!("file-{index}.rs") }),
                )
            })
            .collect::<Vec<_>>();

        let agent_hidden = agent_auxiliary_lines(&progress, false)
            .into_iter()
            .find(|row| row.contains("more tool"))
            .expect("agent hidden line");
        assert_eq!(agent_hidden, "+2 more tool uses (ctrl+o to expand)");

        let skill_hidden = skill_progress_auxiliary_messages(&progress, false)
            .into_iter()
            .find(|row| row.contains("more tool"))
            .expect("skill hidden line");
        assert_eq!(skill_hidden, "+2 more tool uses");
    }

    /// CC renders `tool.userFacingName(data)` (`AssistantToolUseMessage.tsx:110`,
    /// `:178-186`), never the wire name — and the collapsed agent view gets there
    /// by mounting that very component (`AgentTool/UI.tsx:690-700`).
    ///
    /// `GrepTool.ts:161` is `name: 'Grep'`; `:169-171` is
    /// `userFacingName() { return 'Search' }`. This row interpolated
    /// `tool_use.name`, so every nested search read `Grep (…)` on a screen where
    /// the top-level row for the same tool reads `Search (…)`.
    #[test]
    fn collapsed_subagent_rows_render_the_user_facing_name_not_the_wire_name() {
        let progress = vec![subagent_tool_use(
            "toolu_nested_grep",
            "Grep",
            serde_json::json!({ "pattern": "**/*.md", "path": "/repo" }),
        )];
        let rows = agent_auxiliary_lines(&progress, false);
        let [row] = rows.as_slice() else {
            panic!("expected exactly one row, got {rows:?}");
        };
        assert!(row.starts_with("Search("), "row={row}");
        assert!(
            !row.contains("Grep"),
            "wire name leaked into the row: {row}"
        );
    }

    /// The gate and the tally read DIFFERENT things, and this pins the split.
    ///
    /// CC hides a row by returning null from the mounted `MessageComponent`
    /// (`AssistantToolUseMessage.tsx:145-150` on a failed `safeParse`), and
    /// `UI.tsx:687-689` mounts it "without height=1 wrapper so null content …
    /// doesn't leave a blank line". But `hiddenToolUseCount` (`UI.tsx:624-634`)
    /// is a pure content predicate — `content.some(c => c.type === 'tool_use')`
    /// — evaluated over `processedMessages`, which still holds the hidden row.
    ///
    /// So a schema-rejected tool use renders nothing AND still counts. Dropping
    /// it at construction would satisfy the first half and silently shift both
    /// the slice window and the `+N more` tally.
    #[test]
    fn collapsed_subagent_row_gated_on_input_renders_nothing_but_still_counts() {
        // Five tool uses, three shown, so the first two are hidden. Index 0 (a
        // hidden row) and index 4 (a visible one) carry inputs GrepTool's schema
        // rejects — `pattern` is required.
        let progress = (0..5)
            .map(|index| {
                let input = if index == 0 || index == 4 {
                    serde_json::json!({ "path": "/repo" })
                } else {
                    serde_json::json!({ "pattern": format!("p{index}") })
                };
                subagent_tool_use(&format!("toolu_nested_{index}"), "Grep", input)
            })
            .collect::<Vec<_>>();

        let rows = agent_auxiliary_lines(&progress, false);

        // Of the three visible rows one is gated, and it leaves NO line — not a
        // blank one, and not a bare `Search` either.
        assert_eq!(
            rows.iter().filter(|row| row.starts_with("Search")).count(),
            2,
            "a gated row must render no line at all: {rows:?}"
        );
        assert!(
            rows.iter().all(|row| row.trim() != "Search"),
            "a gated row degraded to a bare tool name: {rows:?}"
        );
        // The hidden tally is unchanged by the gate: two hidden rows, both
        // counted, even though one of them would render nothing.
        assert!(
            rows.iter()
                .any(|row| row == "+2 more tool uses (ctrl+o to expand)"),
            "the gate moved the hidden tally: {rows:?}"
        );
    }

    /// The `userFacingName === ''` gate (`AssistantToolUseMessage.tsx:141-143`)
    /// applies here too: `ToolSearchTool.userFacingName: () => ''`, so CC's
    /// mounted component returns null and the nested row is invisible — while
    /// still counting as a tool use, same as the schema gate above.
    #[test]
    fn collapsed_subagent_row_for_a_nonvisual_tool_renders_nothing() {
        let progress = vec![
            subagent_tool_use(
                "toolu_nested_toolsearch",
                "ToolSearch",
                serde_json::json!({ "query": "bash" }),
            ),
            subagent_tool_use(
                "toolu_nested_read",
                "Read",
                serde_json::json!({ "file_path": "src/main.rs" }),
            ),
        ];
        let rows = agent_auxiliary_lines(&progress, false);
        assert_eq!(
            rows,
            vec!["Read(src/main.rs)".to_string()],
            "the nonvisual row should contribute no line: {rows:?}"
        );
    }

    /// Empty text occupies a slice slot but not a `+N more` count. CC keeps
    /// every processed message (`UI.tsx:687-689`) and only the RENDER returns
    /// null; `hiddenToolUseCount` (`:624-634`) is a `tool_use` predicate, so
    /// a text-only row never changes the tally — but it DOES change
    /// `processedMessages.length`, hence `slice(-3)` and WHICH rows show.
    ///
    /// Four Read tool-uses plus a trailing empty text: dropping the text at
    /// construction shrinks the list to 4, hides 1, and shows the first three
    /// Reads plus `+1 more`. Keeping it: 5 slots, hide 2, show the last two
    /// Reads (the empty text is filtered after the slice). The `+N` tally
    /// stays 2 either way — that's the split this pins.
    #[test]
    fn collapsed_empty_text_row_occupies_the_slice_window_without_counting() {
        let mut progress = (0..4)
            .map(|index| {
                subagent_tool_use(
                    &format!("toolu_nested_{index}"),
                    "Read",
                    serde_json::json!({ "file_path": format!("file-{index}.rs") }),
                )
            })
            .collect::<Vec<_>>();
        progress.push(subagent_text("   "));

        let rows = agent_auxiliary_lines(&progress, false);
        assert!(
            rows.iter()
                .all(|row| !row.trim().is_empty() || row.contains("more tool")),
            "empty text must not leave a blank line: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("file-0.rs")),
            "dropping the empty text at construction would have exposed file-0: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("file-2.rs")),
            "the slice window must still show the later reads: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row == "+2 more tool uses (ctrl+o to expand)"),
            "empty text must not change the hidden tally: {rows:?}"
        );
    }

    #[test]
    fn render_tool_use_message_matches_official_null_and_empty_string_outcomes() {
        // CC `renderToolUseMessage` returning null hides the row.
        assert_eq!(
            render_tool_use_message(
                "ToolSearch",
                &serde_json::json!({ "query": "bash" }),
                ToolRenderOptions::default()
            ),
            None
        );
        // A malformed input matches CC's failed `inputSchema.safeParse` and
        // hides the complete tool-use row.
        assert_eq!(
            render_tool_use_message(
                "Read",
                &serde_json::json!({ "unexpected": 1 }),
                ToolRenderOptions::default()
            ),
            None
        );
        assert_eq!(
            render_tool_use_message(
                "FileRead",
                &serde_json::json!({ "file_path": "src/main.rs" }),
                ToolRenderOptions::default()
            ),
            None
        );
    }

    #[test]
    fn render_tool_use_message_never_returns_raw_input_json() {
        let inputs = [
            (
                "Write",
                serde_json::json!({ "file_path": "/tmp/a.txt", "content": "hi" }),
            ),
            (
                "WebFetch",
                serde_json::json!({ "url": "https://example.com", "prompt": "sum" }),
            ),
            ("Bash", serde_json::json!({ "command": "echo hi" })),
            ("Glob", serde_json::json!({ "pattern": "**/*.rs" })),
            ("Unknown", serde_json::json!({ "nested": { "deep": true } })),
        ];
        for (tool_name, input) in inputs {
            let rendered = render_tool_use_message(tool_name, &input, ToolRenderOptions::default())
                .unwrap_or_default();
            assert!(
                !rendered.contains("{\"") && !rendered.contains("\":"),
                "{tool_name} leaked raw input json: {rendered}"
            );
        }
    }

    #[test]
    fn render_tool_use_message_tracks_verbose_like_official_renderers() {
        let input = serde_json::json!({ "url": "https://example.com", "prompt": "summarize" });
        assert_eq!(
            render_tool_use_message("WebFetch", &input, ToolRenderOptions::default()),
            Some("https://example.com".to_string())
        );
        assert_eq!(
            render_tool_use_message(
                "WebFetch",
                &input,
                ToolRenderOptions {
                    verbose: true,
                    ..ToolRenderOptions::default()
                }
            ),
            Some("url: \"https://example.com\", prompt: \"summarize\"".to_string())
        );
    }

    #[test]
    fn assistant_tool_use_min_width_uses_terminal_display_width() {
        assert_eq!(tool_name_min_width("Bash", true), 6);
        assert_eq!(tool_name_min_width("搜索", true), 6);
        assert_eq!(tool_name_min_width("Bash", false), 4);
    }

    /// CC declares `aliases: ['AgentOutputTool', 'BashOutputTool']`
    /// (`TaskOutputTool.tsx:176`) beside the canonical `'TaskOutput'`
    /// (`constants.ts:1`), and `findToolByName` resolves all three to the same
    /// Tool object — so an aliased row must render exactly like the canonical
    /// one.
    ///
    /// The schema gate accepted the aliases while the three dispatches below it
    /// did not, so a row named with a real CC alias rendered as a bare name with
    /// no tag and no summary. The set here matches
    /// `user_tool_result_message/mod.rs:1053-1059`, because the two render paths
    /// must agree on which rows are TaskOutput rows.
    #[test]
    fn taskoutput_aliases_render_like_the_canonical_name() {
        let input = serde_json::json!({ "task_id": "task-abc", "block": false });
        for name in [
            // CC's own three
            "TaskOutput",
            "AgentOutputTool",
            "BashOutputTool",
            // spellings this port invented; accepted so both paths agree
            "TaskOutputTool",
            "BashOutput",
            "AgentOutput",
        ] {
            assert_eq!(
                assistant_tool_use_tag(name, Some(&input)),
                Some("task-abc".to_string()),
                "{name} must carry the id as a tag"
            );
            assert_eq!(
                tool_use_display_name(name, Some(&input)),
                "Task Output",
                "{name} must use the canonical facing name"
            );
            assert_eq!(
                render_tool_use_message(name, &input, ToolRenderOptions::default()),
                Some("non-blocking".to_string()),
                "{name} must reach TaskOutput's own summary"
            );
        }
    }

    /// CC builds the row's summary and its tag from two members that never see
    /// each other (`AssistantToolUseMessage.tsx:196-201`). TaskOutput is the
    /// case that proves it: `renderToolUseMessage` reads only `block`
    /// (`TaskOutputTool.tsx:369-375`) and `renderToolUseTag` reads only
    /// `task_id` (`:377-382`).
    ///
    /// This replaces a test that asserted the opposite — that the tag was
    /// recovered by string-splitting `"non-blocking · task-abc"` back apart —
    /// which froze a Rust invention into a contract.
    #[test]
    fn assistant_tool_use_tag_and_summary_never_derive_from_each_other() {
        use crate::tools::task_output_tool::ui::task_output_tool_use_summary;

        let both = serde_json::json!({ "task_id": "task-abc", "block": false });
        assert_eq!(
            assistant_tool_use_tag("TaskOutput", Some(&both)),
            Some("task-abc".to_string())
        );
        assert_eq!(
            task_output_tool_use_summary(&both),
            Some("non-blocking".to_string()),
            "the summary must not carry the id"
        );

        // `task_id` is required by CC's schema (`TaskOutputTool.tsx:30-43`), so
        // a row without one fails `safeParse` and CC renders nothing
        // (`:88-89`, `:145-150`). The component now runs that same parse
        // (`assistant_tool_use_input_parses`), but this summary keeps its own
        // required-field check for the callers that have no gate — see its doc.
        let no_id = serde_json::json!({ "block": false });
        assert_eq!(assistant_tool_use_tag("TaskOutput", Some(&no_id)), None);
        assert_eq!(task_output_tool_use_summary(&no_id), None);

        // An EMPTY id is a valid `z.string()`, so the row renders — with no tag,
        // since `renderToolUseTag`'s guard is truthiness.
        let empty_id = serde_json::json!({ "task_id": "", "block": false });
        assert_eq!(assistant_tool_use_tag("TaskOutput", Some(&empty_id)), None);
        assert_eq!(
            task_output_tool_use_summary(&empty_id),
            Some("non-blocking".to_string())
        );

        // `block` defaults to true (`const { block = true } = input`).
        let blocking = serde_json::json!({ "task_id": "task-abc" });
        assert_eq!(task_output_tool_use_summary(&blocking), Some(String::new()));
        assert_eq!(
            assistant_tool_use_tag("TaskOutput", Some(&blocking)),
            Some("task-abc".to_string())
        );

        // Tools without the member get no tag even when the input has the field.
        assert_eq!(assistant_tool_use_tag("Bash", Some(&both)), None);
    }

    #[test]
    fn live_read_agent_output_uses_read_owned_tag_without_raw_path_sentinel() {
        let path = crate::utils::task::disk_output::get_task_output_dir()
            .join("task-abc.output")
            .display()
            .to_string();
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Read".to_string(),
                    input: Some(serde_json::json!({"file_path": path.clone()})),
                    status: Some(ToolUseStatus::Succeeded),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Read agent output"), "canvas=\n{text}");
        assert!(text.contains("task-abc"), "canvas=\n{text}");
        assert!(!text.contains(&path), "canvas=\n{text}");

        let malformed = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Read".to_string(),
                    input: Some(serde_json::json!({"path": "src/main.rs"})),
                    status: Some(ToolUseStatus::Succeeded),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(malformed.trim().is_empty(), "canvas=\n{malformed}");
    }

    /// One Agent tool_use the way a real payload carries it: `description` and
    /// `prompt` are both required by the tool's `inputSchema`
    /// (`AgentTool.tsx:164-169`), and CC hides the row without either.
    fn agent_tool_use_input(subagent_type: &str, description: &str) -> serde_json::Value {
        serde_json::json!({
            "description": description,
            "prompt": format!("Inspect {description}"),
            "subagent_type": subagent_type,
        })
    }

    /// CC's ungrouped Agent row reads THREE independent members off the Tool
    /// object, all from the same `param.input`
    /// (`AssistantToolUseMessage.tsx:88-96`): `userFacingName`
    /// (`AgentTool.tsx:130` → `UI.tsx:989-1011`),
    /// `userFacingNameBackgroundColor` (`:131` → `:1013-1024`) and
    /// `renderToolUseMessage` (`:472-483`). Recovering the name by splitting
    /// the rendered summary instead diverges on exactly these inputs.
    #[test]
    fn assistant_tool_use_agent_input_drives_user_facing_name_like_official() {
        use crate::tools::agent_tool::ui::user_facing_name;

        assert_eq!(
            user_facing_name(Some(&agent_tool_use_input("reviewer", "Review auth"))),
            "reviewer"
        );
        // `:1004-1007` — the ONE type that displays as "Agent" despite being set.
        assert_eq!(
            user_facing_name(Some(&agent_tool_use_input("worker", "Run tests"))),
            "Agent"
        );
        // `:1000-1003` — `general-purpose` is compared case-SENSITIVELY, so a
        // differently-cased spelling is an ordinary custom type.
        assert_eq!(
            user_facing_name(Some(&agent_tool_use_input(
                "general-purpose",
                "Review auth"
            ))),
            "Agent"
        );
        assert_eq!(
            user_facing_name(Some(&agent_tool_use_input(
                "General-Purpose",
                "Review auth"
            ))),
            "General-Purpose"
        );
        // `agent` / `task` are not special to CC — they are the TOOL's names,
        // never a subagent type.
        assert_eq!(
            user_facing_name(Some(&agent_tool_use_input("agent", "Review auth"))),
            "agent"
        );
        assert_eq!(
            user_facing_name(Some(&agent_tool_use_input("task", "Review auth"))),
            "task"
        );
        // `input?.subagent_type &&` is JS-truthy, and `undefined` reaches the
        // same trailing `return 'Agent'` (the row passes `data` = undefined on
        // a failed `safeParse`, `AssistantToolUseMessage.tsx:90`).
        assert_eq!(
            user_facing_name(Some(&agent_tool_use_input("", "Review auth"))),
            "Agent"
        );
        assert_eq!(user_facing_name(Some(&serde_json::json!({}))), "Agent");
        assert_eq!(user_facing_name(None), "Agent");

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Agent".to_string(),
                    input: Some(agent_tool_use_input("reviewer", "Review auth")),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("reviewer(Review auth)"), "canvas=\n{text}");
        assert!(!text.contains("Agent(reviewer"), "canvas=\n{text}");
    }

    /// CC `AgentTool/UI.tsx:485-512#renderToolUseTag` — the model is a separate
    /// dim tag beside the row, shown only when it resolves to something other
    /// than the main-loop model.
    #[test]
    fn assistant_tool_use_agent_model_renders_as_the_official_tag() {
        use crate::tools::agent_tool::ui::render_tool_use_tag;

        let with_model = |model: serde_json::Value| {
            let mut input = agent_tool_use_input("reviewer", "Review auth");
            input["model"] = model;
            input
        };

        // `if (input.model)` is JS-truthy: absent and empty both render nothing.
        assert_eq!(
            render_tool_use_tag(&agent_tool_use_input("reviewer", "Review auth")),
            None
        );
        assert_eq!(
            render_tool_use_tag(&with_model(serde_json::json!(""))),
            None
        );
        // `agentModel !== mainModel` — the resolved main-loop model is never
        // repeated on the row.
        let main_model = crate::utils::model::model::get_main_loop_model();
        assert_eq!(
            render_tool_use_tag(&with_model(serde_json::json!(main_model))),
            None
        );
        // A different alias resolves through `parseUserSpecifiedModel` and
        // displays through `renderModelName`.
        let opus_tag = render_tool_use_tag(&with_model(serde_json::json!("opus")))
            .expect("opus differs from the default main-loop model");
        assert!(opus_tag.contains("Opus"), "tag={opus_tag}");

        let tagged_text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Agent".to_string(),
                    input: Some(with_model(serde_json::json!("opus"))),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(
            tagged_text.contains("reviewer(Review auth)"),
            "canvas=\n{tagged_text}"
        );
        assert!(tagged_text.contains(&opus_tag), "canvas=\n{tagged_text}");
        // The tag is chrome beside the row, never spliced into the summary.
        assert!(!tagged_text.contains("[model:"), "canvas=\n{tagged_text}");
    }

    #[test]
    fn assistant_agent_tool_use_applies_official_agent_color_context() {
        let theme = *crate::utils::theme::current();
        crate::tools::agent_tool::agent_color_manager::set_agent_color(
            "reviewer",
            Some(crate::tools::agent_tool::agent_color_manager::AgentColorName::Purple),
        );
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                AssistantToolUseMessage(
                    tool_name: "Agent".to_string(),
                    input: Some(agent_tool_use_input("reviewer", "Review auth")),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);
        crate::tools::agent_tool::agent_color_manager::set_agent_color("reviewer", None);
        let text = canvas.to_string();
        let (column, row) = find_text_cell(&canvas, "reviewer").expect("reviewer cell");
        let first_cell = canvas.cell(column, row).expect("reviewer first cell");

        assert!(text.contains("reviewer(Review auth)"), "canvas=\n{text}");
        assert_eq!(first_cell.background_color, Some(theme.agent_purple));
    }

    #[test]
    fn assistant_agent_tool_use_uses_built_in_active_agent_colors() {
        let theme = *crate::utils::theme::current();
        crate::tools::agent_tool::agent_color_manager::set_agent_color(
            "verification",
            Some(crate::tools::agent_tool::agent_color_manager::AgentColorName::Red),
        );
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                AssistantToolUseMessage(
                    tool_name: "Agent".to_string(),
                    input: Some(agent_tool_use_input("verification", "Check implementation")),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);
        crate::tools::agent_tool::agent_color_manager::set_agent_color("verification", None);
        let text = canvas.to_string();
        let (column, row) = find_text_cell(&canvas, "verification").expect("verification cell");
        let first_cell = canvas.cell(column, row).expect("verification first cell");

        assert!(
            text.contains("verification(Check implementation)"),
            "canvas=\n{text}"
        );
        assert_eq!(first_cell.background_color, Some(theme.agent_red));
    }

    /// CC `AgentTool/UI.tsx:1013-1024#userFacingNameBackgroundColor` runs on
    /// the RAW `subagent_type`, while `userFacingName` (`:1004-1007`) rewrites
    /// `worker` to "Agent" — so a `worker` row shows the bare name ON its
    /// assigned background. The name is not, and cannot be, the colour key.
    #[test]
    fn assistant_agent_tool_use_colors_worker_rows_despite_the_agent_facing_name() {
        use crate::tools::agent_tool::agent_color_manager::{AgentColorName, set_agent_color};
        use crate::tools::agent_tool::ui::user_facing_name_background_color;

        let theme = *crate::utils::theme::current();
        set_agent_color("worker", Some(AgentColorName::Cyan));
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                AssistantToolUseMessage(
                    tool_name: "Agent".to_string(),
                    input: Some(agent_tool_use_input("worker", "Run tests")),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);
        set_agent_color("worker", None);
        let text = canvas.to_string();
        let (column, row) = find_text_cell(&canvas, "Agent").expect("Agent cell");
        let first_cell = canvas.cell(column, row).expect("Agent first cell");

        assert!(text.contains("Agent(Run tests)"), "canvas=\n{text}");
        assert_eq!(first_cell.background_color, Some(theme.agent_cyan));

        // `if (!input?.subagent_type) return undefined` — one JS-truthy guard
        // over the absent input, the absent key, and the empty string.
        assert_eq!(
            user_facing_name_background_color(Some(&agent_tool_use_input("", "Run tests"))),
            None
        );
        assert_eq!(
            user_facing_name_background_color(Some(&serde_json::json!({}))),
            None
        );
        assert_eq!(user_facing_name_background_color(None), None);
        // `general-purpose` is exempt inside `getAgentColor`
        // (`agentColorManager.ts:36-39`), even with a colour assigned.
        set_agent_color("general-purpose", Some(AgentColorName::Cyan));
        let general_purpose = user_facing_name_background_color(Some(&agent_tool_use_input(
            "general-purpose",
            "Run tests",
        )));
        set_agent_color("general-purpose", None);
        assert_eq!(general_purpose, None);
    }

    #[test]
    fn assistant_tool_use_animation_obeys_message_row_gate() {
        assert!(tool_use_loader_should_animate(ToolUseStatus::Running, true));
        assert!(!tool_use_loader_should_animate(
            ToolUseStatus::Running,
            false
        ));
        assert!(!tool_use_loader_should_animate(ToolUseStatus::Queued, true));
        assert!(!tool_use_loader_should_animate(
            ToolUseStatus::Succeeded,
            true
        ));
    }

    #[test]
    fn assistant_tool_use_transparent_wrapper_hides_non_running_rows_like_official() {
        for status in [
            ToolUseStatus::Queued,
            ToolUseStatus::Succeeded,
            ToolUseStatus::Failed,
        ] {
            let text = element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    AssistantToolUseMessage(
                        tool_name: "REPL".to_string(),
                        description: "transparent wrapper".to_string(),
                        status: Some(status),
                        progress_messages: vec![ToolUseProgressMessage::QueryUpdate {
                            query: "ignored".to_string(),
                        }],
                        is_transparent_wrapper: true,
                        can_animate: false,
                        verbose: false,
                        is_transcript_mode: false,
                    )
                }
            }
            .render(None)
            .to_string();

            assert!(text.trim().is_empty(), "status={status:?} canvas=\n{text}");
        }
    }

    #[test]
    fn assistant_tool_use_transparent_wrapper_renders_running_progress_without_chrome() {
        // CC AssistantToolUseMessage.tsx:124-139 — a running transparent
        // wrapper renders the wrapped tool's own progress renderer with no
        // tool chrome. The payload is the tool's real wire shape
        // (`bash_progress`, BashTool.tsx:900-912).
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    description: "transparent wrapper".to_string(),
                    status: Some(ToolUseStatus::Running),
                    progress_messages: vec![ToolUseProgressMessage::BashProgress {
                        output: "VM line one\nVM line two".to_string(),
                        full_output: "VM line one\nVM line two".to_string(),
                        elapsed_time_seconds: 3,
                        total_lines: 2,
                        total_bytes: None,
                        task_id: None,
                        timeout_ms: None,
                    }],
                    is_transparent_wrapper: true,
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("VM line one"), "canvas=\n{text}");
        assert!(text.contains("VM line two"), "canvas=\n{text}");
        assert!(!text.contains("Bash"), "canvas=\n{text}");
        assert!(!text.contains(BLACK_CIRCLE), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_wrapper_without_progress_renderer_renders_nothing() {
        // CC renders `tool.renderToolUseProgressMessage?.(...) ?? null`
        // (AssistantToolUseMessage.tsx:283-290), so a running wrapper whose
        // tool ships no progress renderer renders nothing at all: no chrome,
        // no dot, no invented output rows.
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "MockWrapper".to_string(),
                    description: "transparent wrapper".to_string(),
                    status: Some(ToolUseStatus::Running),
                    progress_messages: vec![ToolUseProgressMessage::QueryUpdate {
                        query: "ignored".to_string(),
                    }],
                    is_transparent_wrapper: true,
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.trim().is_empty(), "canvas=\n{text}");
        assert!(!text.contains("MockWrapper"), "canvas=\n{text}");
    }

    /// An AppStore whose `pendingWorkerRequest` names `tool_use_id`, as a swarm
    /// worker's store does while it waits on the leader
    /// (swarmWorkerHandler.ts:62-65).
    fn store_with_pending_worker_request(tool_use_id: &str) -> crate::state::store::AppStore {
        crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState {
                pending_worker_request: Some(std::sync::Arc::new(
                    crate::hooks::use_inbox_poller::PendingWorkerRequest {
                        tool_name: "Bash".to_string(),
                        tool_use_id: tool_use_id.to_string(),
                        description: "echo permission-gated".to_string(),
                    },
                )),
                ..Default::default()
            },
            None,
        )
    }

    #[test]
    fn assistant_tool_use_waiting_permission_row_takes_priority() {
        // CC :204-215 — only a running row (`!isResolved && !isQueued`); a
        // queued one shows its queued message (:232).
        assert_eq!(
            auxiliary_message("Read", ToolUseStatus::Running, false, false, true),
            some_message("Waiting for permission…")
        );
        assert_eq!(
            auxiliary_message("Bash", ToolUseStatus::Queued, false, false, true),
            some_message("Waiting…")
        );
        assert_eq!(
            auxiliary_message("Bash", ToolUseStatus::Succeeded, false, false, true),
            None
        );

        let render = |tool_use_id: &str| {
            let store = store_with_pending_worker_request("toolu_waiting");
            element! {
                ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                    ContextProvider(value: Context::owned(store)) {
                        AssistantToolUseMessage(
                            tool_use_id: Some(tool_use_id.to_string()),
                            tool_name: "Bash".to_string(),
                            description: "echo permission-gated".to_string(),
                            status: Some(ToolUseStatus::Running),
                            can_animate: false,
                            verbose: false,
                            is_transcript_mode: false,
                        )
                    }
                }
            }
            .render(None)
            .to_string()
        };

        // CC :122 `pendingWorkerRequest?.toolUseId === param.id`, read by the
        // row from AppState.
        let text = render("toolu_waiting");
        assert!(text.contains("Waiting for permission…"), "canvas=\n{text}");
        let other = render("toolu_other");
        assert!(!other.contains("Waiting for permission…"), "canvas=\n{other}");
    }

    #[test]
    fn assistant_tool_use_classifier_row_takes_priority_for_running_tools() {
        assert_eq!(
            auxiliary_message("Bash", ToolUseStatus::Running, true, false, true),
            some_message("Bash classifier checking…")
        );
        assert_eq!(
            auxiliary_message("Bash", ToolUseStatus::Running, true, true, true),
            some_message("Auto classifier checking…")
        );
        assert_eq!(
            auxiliary_message("Bash", ToolUseStatus::Queued, true, true, false),
            some_message("Waiting…")
        );

        let classifier_state = ClassifierApprovalsState::start_checking(
            crate::utils::classifier_approvals::ClassifierChecking {
                tool_use_id: "toolu_classifier".to_string(),
                is_auto: false,
            },
        );
        let store = store_with_pending_worker_request("toolu_classifier");
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value: Context::owned(store)) {
                    ContextProvider(value: Context::owned(classifier_state)) {
                        AssistantToolUseMessage(
                            tool_use_id: Some("toolu_classifier".to_string()),
                            tool_name: "Bash".to_string(),
                            description: "rm -rf tmp".to_string(),
                            status: Some(ToolUseStatus::Running),
                            can_animate: false,
                            verbose: false,
                            is_transcript_mode: false,
                        )
                    }
                }
            }
        }
        .render(None)
        .to_string();

        assert!(
            text.contains("Bash classifier checking…"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Waiting for permission…"), "canvas=\n{text}");
        assert!(!text.contains("Running…"), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_renders_bash_queued_and_running_auxiliary_rows() {
        let queued = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    description: "echo hi".to_string(),
                    status: Some(ToolUseStatus::Queued),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        let running = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    description: "echo hi".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(queued.contains("Waiting…"), "canvas=\n{queued}");
        assert!(running.contains("Running…"), "canvas=\n{running}");
    }

    #[test]
    fn assistant_tool_use_renders_task_output_tag_outside_parentheses() {
        // The wire name and the raw input, exactly as every mount site passes
        // them (`message.rs:396`, `user_tool_result_message/mod.rs:345`). This
        // used to pass the DISPLAY name plus a pre-rendered `"task-abc"`
        // summary, which was the only thing that ever reached the reverse-parse
        // branch — production never did.
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "TaskOutput".to_string(),
                    input: Some(serde_json::json!({ "task_id": "task-abc" })),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        // The id is a tag, so it trails the name unparenthesised; `block`
        // defaults to true, so `renderToolUseMessage` is `''` and no parens
        // render at all (`AssistantToolUseMessage.tsx:196-201`).
        assert!(text.contains("Task Output task-abc"), "canvas=\n{text}");
        assert!(!text.contains("(task-abc)"), "canvas=\n{text}");
        let tool_row = text
            .lines()
            .find(|line| line.contains("Task Output"))
            .unwrap_or_default();
        assert!(!tool_row.contains('('), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_renders_official_no_progress_rows_for_additional_tools() {
        assert_eq!(
            auxiliary_message("PowerShell", ToolUseStatus::Queued, false, false, false),
            some_message("Waiting…")
        );
        assert_eq!(
            auxiliary_message("PowerShell", ToolUseStatus::Running, false, false, false),
            some_message("Running…")
        );
        assert_eq!(
            auxiliary_message("Fetch", ToolUseStatus::Running, false, false, false),
            some_message("Fetching…")
        );
        assert_eq!(
            auxiliary_message("WebFetch", ToolUseStatus::Running, false, false, false),
            some_message("Fetching…")
        );
        assert_eq!(
            auxiliary_message("mcp", ToolUseStatus::Running, false, false, false),
            some_message("Running…")
        );
        assert_eq!(
            auxiliary_message(
                "mcp__filesystem__read",
                ToolUseStatus::Running,
                false,
                false,
                false
            ),
            some_message("Running…")
        );
        assert_eq!(
            auxiliary_message("Task Output", ToolUseStatus::Running, false, false, false),
            some_message("Waiting for task (esc to give additional instructions)")
        );
        // Agent's no-progress `Initializing…` (`AgentTool/UI.tsx:532-538`) is
        // its RENDERER's own first return, and that renderer is a component
        // since K4-G1 — so the string half must stay silent for it or the row
        // would render twice. The element half answers instead:
        assert_eq!(
            auxiliary_message("Agent", ToolUseStatus::Running, false, false, false),
            None
        );
        assert_eq!(agent_auxiliary_lines(&[], false), vec!["Initializing…"]);
        // Skill's own renderer is still the string projection.
        assert_eq!(
            auxiliary_message("Skill", ToolUseStatus::Running, false, false, false),
            some_message("Initializing…")
        );
        assert_eq!(
            auxiliary_message("Fetch", ToolUseStatus::Queued, false, false, false),
            None
        );
        assert_eq!(
            auxiliary_message("Web Search", ToolUseStatus::Running, false, false, false),
            None
        );
    }

    #[test]
    fn assistant_tool_use_renders_web_search_progress_data_rows() {
        let progress = vec![
            ToolUseProgressMessage::QueryUpdate {
                query: "rust async runtime".to_string(),
            },
            ToolUseProgressMessage::SearchResultsReceived {
                query: "rust async runtime".to_string(),
                result_count: 7,
            },
        ];
        assert_eq!(
            tool_use_auxiliary_message(
                "Web Search",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &progress,
            ),
            some_message("Found 7 results for \"rust async runtime\"")
        );

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Web Search".to_string(),
                    description: "\"rust async runtime\"".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(
            text.contains("Found 7 results for \"rust async runtime\""),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Running…"),
            "Web Search should not invent no-progress rows when official progress data exists; canvas=\n{text}"
        );
    }

    #[test]
    fn assistant_tool_use_renders_shell_progress_data_rows() {
        let progress = vec![ToolUseProgressMessage::BashProgress {
            output: "one\ntwo\nthree\nfour\nfive\nsix\nseven".to_string(),
            full_output: "\u{1b}[31mone\u{1b}[0m\ntwo\nthree\nfour\nfive\nsix\nseven".to_string(),
            elapsed_time_seconds: 65,
            total_lines: 7,
            total_bytes: Some(1536),
            task_id: None,
            timeout_ms: Some(120_000),
        }];
        assert_eq!(
            tool_use_auxiliary_messages(
                "Bash",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &progress,
                ToolUseProgressOptions::default(),
            ),
            vec![
                "three\nfour\nfive\nsix\nseven".to_string(),
                "~7 lines (1m 5s · timeout 2m) 1.5KB".to_string(),
            ]
        );
        assert_eq!(
            tool_use_auxiliary_messages(
                "PowerShell",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &progress,
                ToolUseProgressOptions {
                    verbose: true,
                    ..Default::default()
                },
            ),
            vec![
                "one\ntwo\nthree\nfour\nfive\nsix\nseven".to_string(),
                "(1m 5s · timeout 2m) 1.5KB".to_string(),
            ]
        );

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    description: "npm test".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("three"), "canvas=\n{text}");
        assert!(text.contains("seven"), "canvas=\n{text}");
        assert!(text.contains("~7 lines"), "canvas=\n{text}");
        assert!(text.contains("1.5KB"), "canvas=\n{text}");
        assert!(!text.contains("\u{1b}[31m"), "canvas=\n{text}");
        assert!(!text.contains("Running…"), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_renders_shell_progress_running_time_without_output() {
        let progress = vec![ToolUseProgressMessage::BashProgress {
            output: "".to_string(),
            full_output: "".to_string(),
            elapsed_time_seconds: 1,
            total_lines: 0,
            total_bytes: None,
            task_id: None,
            timeout_ms: Some(90_000),
        }];
        assert_eq!(
            tool_use_auxiliary_messages(
                "Bash",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &progress,
                ToolUseProgressOptions::default(),
            ),
            vec!["Running… (1s · timeout 1m 30s)".to_string()]
        );
    }

    #[test]
    fn assistant_tool_use_renders_task_output_progress_data_rows() {
        let progress = vec![ToolUseProgressMessage::WaitingForTask {
            task_description: "Running regression suite".to_string(),
            task_type: "local_bash".to_string(),
        }];
        assert_eq!(
            tool_use_auxiliary_messages(
                "Task Output",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &progress,
                ToolUseProgressOptions::default(),
            ),
            vec![
                "Running regression suite".to_string(),
                "Waiting for task (esc to give additional instructions)".to_string(),
            ]
        );

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Task Output".to_string(),
                    description: "task-abc".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Running regression suite"), "canvas=\n{text}");
        assert!(
            text.contains("Waiting for task (esc to give additional instructions)"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Initializing…"), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_renders_subagent_progress_data_rows() {
        let progress = vec![
            subagent_text("Planning implementation"),
            subagent_tool_use(
                "toolu_nested_read",
                "Read",
                serde_json::json!({ "file_path": "src/main.rs" }),
            ),
            subagent_tool_use(
                "toolu_nested_bash",
                "Bash",
                serde_json::json!({ "command": "cargo test" }),
            ),
            subagent_text("Summarizing result"),
        ];
        // The Agent renderer is a component now, so its rows come off a canvas
        // (see `agent_auxiliary_lines`) — and its `Name(summary)` shape is the
        // mounted component's two adjacent boxes, without the space the
        // retired string projection inserted.
        //
        // `Running…` is the nested Bash row's OWN progress renderer
        // (`BashTool/UI.tsx:141-147`, the `!lastProgress` branch): CC mounts
        // the nested tool use with `progressMessagesForMessage={[]}`
        // (`AgentTool/UI.tsx:698`) and an unresolved id, so the row is neither
        // queued nor resolved and its renderer runs. The string projection
        // never reached the nested tool's renderer at all, so this row is new.
        assert_eq!(
            agent_auxiliary_lines(&progress, false),
            vec![
                "Read(src/main.rs)".to_string(),
                "Bash(cargo test)".to_string(),
                "Running…".to_string(),
                "Summarizing result".to_string(),
            ]
        );

        let skill_rows = tool_use_auxiliary_messages(
            "Skill",
            ToolUseStatus::Running,
            false,
            false,
            false,
            &progress,
            ToolUseProgressOptions::default(),
        );
        // CC SkillTool/UI.tsx:127-131 renders the hidden-count line WITHOUT a
        // ctrl+o hint; only the agent renderer appends one
        // (AgentTool/UI.tsx:712-717, pinned by
        // `only_the_agent_hidden_line_carries_the_ctrl_o_hint`).
        assert_eq!(
            skill_rows.last().map(String::as_str),
            Some("+1 more tool use")
        );

        // CC SkillTool/UI.tsx:94-96 — the SKILL window axis is `verbose`
        // (the Agent axis is `isTranscriptMode`, UI.tsx:610-612; pinned by
        // `agent_collapsed_window_slices_on_transcript_not_verbose`).
        let verbose_rows = tool_use_auxiliary_messages(
            "Skill",
            ToolUseStatus::Running,
            false,
            false,
            false,
            &progress,
            ToolUseProgressOptions {
                verbose: true,
                ..Default::default()
            },
        );
        assert_eq!(
            verbose_rows.first().map(String::as_str),
            Some("Planning implementation")
        );
        assert!(!verbose_rows.iter().any(|row| row.contains("more tool")));

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Agent".to_string(),
                    description: "investigate parity".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Read(src/main.rs)"), "canvas=\n{text}");
        assert!(text.contains("Bash(cargo test)"), "canvas=\n{text}");
        assert!(!text.contains("+1 more tool use"), "canvas=\n{text}");
        assert!(!text.contains("Initializing…"), "canvas=\n{text}");
    }

    /// Typed nested-result display, exercised through SKILL: CC's agent
    /// renderer drops every user row (UI.tsx:129-135), so the tool_result
    /// projection is only visible on the skill renderer
    /// (SkillTool/UI.tsx:94-96, no filter) and in the transcript's own
    /// verbose channel. Agent-side filtering is pinned by
    /// `agent_progress_drops_tool_result_rows_while_skill_keeps_them`.
    #[test]
    fn assistant_tool_use_renders_subagent_tool_result_progress_with_typed_display() {
        // The result row's tool identity now comes from the tool_use it
        // answers, resolved through the lookups exactly like CC
        // (`buildSubagentLookups` → `MessageComponent`), so a well-formed
        // fixture carries both halves.
        let progress = vec![
            subagent_tool_use(
                "toolu_nested_read",
                "Read",
                serde_json::json!({ "file_path": "src/auth.rs" }),
            ),
            subagent_tool_result(
                "toolu_nested_read",
                "raw nested read payload should stay hidden",
                // The raw rides the DTO.
                Some(serde_json::json!({
                    "type": "text",
                    "file": {
                        "filePath": "src/auth.rs",
                        "content": "",
                        "numLines": 12,
                        "startLine": 1,
                        "totalLines": 12
                    }
                })),
            ),
            subagent_text("Summarizing result"),
        ];
        assert_eq!(
            tool_use_auxiliary_messages(
                "Skill",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &progress,
                ToolUseProgressOptions::default(),
            ),
            vec![
                "Read (src/auth.rs)".to_string(),
                "Read 12 lines".to_string(),
                "Summarizing result".to_string(),
            ]
        );

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Skill".to_string(),
                    description: "reviewer: Inspect nested result".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Read 12 lines"), "canvas=\n{text}");
        assert!(
            !text.contains("raw nested read payload should stay hidden"),
            "typed display should own visible nested result text; canvas=\n{text}"
        );
        assert!(!text.contains("Initializing…"), "canvas=\n{text}");
    }

    #[test]
    fn live_edit_tool_use_uses_update_name_and_path_instead_of_json() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let input = serde_json::json!({
            "file_path": path.display().to_string(),
            "old_string": "old",
            "new_string": "new"
        });
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Edit".to_string(),
                    input: Some(input),
                    status: Some(ToolUseStatus::Succeeded),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Update"), "canvas=\n{text}");
        assert!(text.contains("src/main.rs"), "canvas=\n{text}");
        assert!(!text.contains("old_string"), "canvas=\n{text}");
        assert!(!text.contains("new_string"), "canvas=\n{text}");
    }

    /// CC `AgentTool/UI.tsx:624-635` counts hidden TOOL USES, not hidden rows:
    /// `count(hiddenMessages, m => data.message.message.content.some(c =>
    /// c.type === 'tool_use'))`. A hidden text row therefore adds nothing.
    ///
    /// (This replaces the old `SubagentOperationSummary` test: CC's
    /// `SummaryMessage` is local to `processProgressMessages` and only reachable
    /// on the ant-only branch, so the external build has no summary row and
    /// Rust had no producer — see the note where the payload variant was.)
    #[test]
    fn hidden_agent_rows_count_tool_uses_not_rows() {
        let progress = vec![
            subagent_text("Collecting file facts"),
            subagent_tool_use(
                "toolu_nested_main",
                "Read",
                serde_json::json!({ "file_path": "src/main.rs" }),
            ),
            subagent_text("Reviewing auth flow"),
            subagent_tool_use(
                "toolu_nested_auth",
                "Read",
                serde_json::json!({ "file_path": "src/auth.rs" }),
            ),
            subagent_text("Summarizing result"),
        ];
        assert_eq!(
            agent_auxiliary_lines(&progress, false),
            vec![
                "Reviewing auth flow".to_string(),
                "Read(src/auth.rs)".to_string(),
                "Summarizing result".to_string(),
                // Hidden: the text row plus ONE tool use.
                "+1 more tool use (ctrl+o to expand)".to_string(),
            ]
        );

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Agent".to_string(),
                    description: "reviewer: Inspect progress".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    progress_messages: progress,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Read(src/auth.rs)"), "canvas=\n{text}");
        assert!(
            text.contains("+1 more tool use (ctrl+o to expand)"),
            "canvas=\n{text}"
        );
        assert!(!text.contains("Initializing…"), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_renders_mcp_progress_data_rows() {
        // Wire fixtures follow the CC producers: `status`/`serverName`/
        // `toolName` are always present (services/mcp/client.ts:3104-3112);
        // the UI reads only progress/total/progressMessage (MCPTool/UI.tsx:82).
        let with_total = vec![ToolUseProgressMessage::McpProgress {
            status: "progress".to_string(),
            server_name: "server".to_string(),
            tool_name: "tool".to_string(),
            elapsed_time_ms: None,
            progress: Some(3.0),
            total: Some(10.0),
            progress_message: Some("Uploading files".to_string()),
        }];
        assert_eq!(
            tool_use_auxiliary_messages(
                "mcp__server__tool",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &with_total,
                ToolUseProgressOptions::default(),
            ),
            vec![
                "Uploading files".to_string(),
                format!("{} 30%", progress_bar_text(0.3, 20)),
            ]
        );

        let without_total = vec![ToolUseProgressMessage::McpProgress {
            status: "progress".to_string(),
            server_name: "server".to_string(),
            tool_name: "tool".to_string(),
            elapsed_time_ms: None,
            progress: Some(42.0),
            total: None,
            progress_message: None,
        }];
        assert_eq!(
            tool_use_auxiliary_message(
                "mcp",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &without_total,
            ),
            some_message("Processing… 42")
        );

        let without_progress = vec![ToolUseProgressMessage::McpProgress {
            status: "started".to_string(),
            server_name: "server".to_string(),
            tool_name: "tool".to_string(),
            elapsed_time_ms: None,
            progress: None,
            total: Some(10.0),
            progress_message: Some("Ignored until numeric progress".to_string()),
        }];
        assert_eq!(
            tool_use_auxiliary_message(
                "mcp",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &without_progress,
            ),
            some_message("Running…")
        );

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "mcp__server__tool".to_string(),
                    description: "query: \"status\"".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    progress_messages: with_total,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Uploading files"), "canvas=\n{text}");
        assert!(text.contains("30%"), "canvas=\n{text}");
        assert_eq!(text.matches('⎿').count(), 1, "canvas=\n{text}");
        assert!(!text.contains("Running…"), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_mcp_progress_renderer_never_returns_null() {
        // CC `MCPTool/UI.tsx:74-80` — `if (!lastProgress?.data)` renders
        // `Running…`, so an empty progress list and a foreign progress variant
        // (every MCP field would be `undefined`) are outputs of the MCP
        // renderer itself, not of the caller's no-progress fallback.
        for progress_messages in [
            Vec::new(),
            vec![ToolUseProgressMessage::QueryUpdate {
                query: "not an mcp payload".to_string(),
            }],
        ] {
            assert_eq!(
                tool_use_progress_auxiliary_messages(
                    "mcp__server__tool",
                    &progress_messages,
                    ToolUseProgressOptions::default(),
                ),
                Some(vec!["Running…".to_string()])
            );
        }

        // The transparent in-flight wrapper reads the same renderer, so it
        // shows the row instead of collapsing to a zero-size view.
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "mcp__server__tool".to_string(),
                    description: "query: \"status\"".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();
        assert!(text.contains("Running…"), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_mcp_empty_progress_message_follows_the_official_operators() {
        // CC `:110` is NULLISH (`progressMessage ?? …`), so an empty message
        // wins over `Processing… N` and renders an empty row.
        let empty_no_total = vec![ToolUseProgressMessage::McpProgress {
            status: "progress".to_string(),
            server_name: "server".to_string(),
            tool_name: "tool".to_string(),
            elapsed_time_ms: None,
            progress: Some(7.0),
            total: None,
            progress_message: Some(String::new()),
        }];
        assert_eq!(
            tool_use_auxiliary_messages(
                "mcp",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &empty_no_total,
                ToolUseProgressOptions::default(),
            ),
            vec![String::new()]
        );

        // CC `:98` is truthiness (`progressMessage && …`), so the same empty
        // message contributes no line above the bar.
        let empty_with_total = vec![ToolUseProgressMessage::McpProgress {
            status: "progress".to_string(),
            server_name: "server".to_string(),
            tool_name: "tool".to_string(),
            elapsed_time_ms: None,
            progress: Some(1.0),
            total: Some(2.0),
            progress_message: Some(String::new()),
        }];
        assert_eq!(
            tool_use_auxiliary_messages(
                "mcp",
                ToolUseStatus::Running,
                false,
                false,
                false,
                &empty_with_total,
                ToolUseProgressOptions::default(),
            ),
            vec![format!("{} 50%", progress_bar_text(0.5, 20))]
        );
    }

    // `assistant_tool_use_renders_repl_progress_data_rows` and
    // `assistant_tool_use_renders_workflow_progress_data_rows` are gone with
    // the `ReplProgress`/`WorkflowProgress` variants: CC has no
    // `repl`/`workflow` progress payload producer — the shapes existed only in
    // the generated stub `types/tools.ts`.

    #[test]
    fn assistant_tool_use_does_not_invent_auxiliary_rows_for_generic_tools() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Read".to_string(),
                    description: "src/main.rs".to_string(),
                    status: Some(ToolUseStatus::Queued),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(!text.contains("Waiting…"), "canvas=\n{text}");
        assert!(!text.contains("Running…"), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_suppresses_empty_tool_name_like_official() {
        assert!(!assistant_tool_use_should_render(""));
        assert!(!assistant_tool_use_should_render("ToolSearch"));
        assert!(assistant_tool_use_should_render("Bash"));

        // `userFacingName` is the ONLY question this layer asks
        // (`AssistantToolUseMessage.tsx:141-143`). `ConfigTool.ts:83` returns
        // `'Config'` and readMcpResource likewise, so neither may be filtered
        // here — their `!input.setting` / `!input.server` checks belong to
        // `renderToolUseMessage` at `:145-149`. Passing `None` used to answer
        // the wrong question and hide every such row.
        assert!(assistant_tool_use_should_render("Config"));
        assert!(assistant_tool_use_should_render("ReadMcpResourceTool"));

        // The lower layer still rejects, and still only when it has the input.
        assert!(assistant_tool_use_is_nonvisual(
            "Config",
            Some(&serde_json::json!({}))
        ));
        assert!(!assistant_tool_use_is_nonvisual(
            "Config",
            Some(&serde_json::json!({ "setting": "theme" }))
        ));
        assert!(assistant_tool_use_is_nonvisual(
            "ReadMcpResourceTool",
            Some(&serde_json::json!({ "server": "memory" }))
        ));
        assert!(!assistant_tool_use_is_nonvisual(
            "ReadMcpResourceTool",
            Some(&serde_json::json!({ "server": "memory", "uri": "mem://x" }))
        ));

        // Both CC guards are truthiness, not presence: `!input.setting`
        // (`ConfigTool/UI.tsx:8`) and `!input.uri || !input.server`
        // (`ReadMcpResourceTool/UI.tsx:15`). An empty string is falsy there, so
        // the row is hidden; a presence-only check rendered `Getting ` instead.
        // Both inputs pass their `z.string()` schemas, so the render gate lets
        // them through and this is the layer that has to answer.
        assert!(assistant_tool_use_is_nonvisual(
            "Config",
            Some(&serde_json::json!({ "setting": "" }))
        ));
        assert!(assistant_tool_use_input_parses(
            "Config",
            Some(&serde_json::json!({ "setting": "" }))
        ));
        assert!(assistant_tool_use_is_nonvisual(
            "ReadMcpResourceTool",
            Some(&serde_json::json!({ "server": "memory", "uri": "" }))
        ));
        assert!(assistant_tool_use_input_parses(
            "ReadMcpResourceTool",
            Some(&serde_json::json!({ "server": "memory", "uri": "" }))
        ));

        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: String::new(),
                    description: "transparent wrapper".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: true,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert_eq!(text.trim(), "", "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_can_hide_dot_like_official_should_show_dot_false() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    description: "echo hi".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: true,
                    should_show_dot: Some(false),
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.starts_with("Bash(echo hi)"), "canvas=\n{text}");
        assert!(!text.starts_with(BLACK_CIRCLE), "canvas=\n{text}");
        assert!(text.contains("Running…"), "canvas=\n{text}");
    }

    #[test]
    fn assistant_tool_use_uses_fixed_loader_cell_and_bold_tool_name() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    description: "echo hi".to_string(),
                    status: Some(ToolUseStatus::Running),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);

        let rendered = canvas.to_string();
        assert!(rendered.starts_with(BLACK_CIRCLE));
        assert!(rendered.contains("Bash(echo hi)"));
        let dot_style = canvas.resolved_text_style(0, 0).expect("dot style");
        // Running + unresolved: official dimColor, no explicit color. `dim` is
        // its own attribute, so the weight stays Normal.
        assert_eq!(dot_style.color, None);
        assert!(dot_style.dim);
        assert_eq!(dot_style.weight, Weight::Light);
        let title_style = canvas.resolved_text_style(2, 0).expect("title style");
        assert_eq!(title_style.color, None);
        assert_eq!(title_style.weight, Weight::Bold);
        assert!(!title_style.dim, "the dim dot must not bleed into the name");
    }

    #[test]
    fn assistant_tool_use_queued_dot_does_not_dim_tool_title() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AssistantToolUseMessage(
                    tool_name: "Bash".to_string(),
                    description: "echo hi".to_string(),
                    status: Some(ToolUseStatus::Queued),
                    can_animate: false,
                    verbose: false,
                    is_transcript_mode: false,
                )
            }
        }
        .render(None);

        let dot_style = canvas.resolved_text_style(0, 0).expect("dot style");
        // Queued: official `<Text dimColor>` — default fg + dim, not inactive.
        assert_eq!(dot_style.color, None);
        assert!(dot_style.dim);
        assert_eq!(dot_style.weight, Weight::Light);
        let title_style = canvas.resolved_text_style(2, 0).expect("title style");
        assert_eq!(title_style.color, None);
        assert_eq!(title_style.weight, Weight::Bold);
        assert!(!title_style.dim, "the dim dot must not bleed into the name");
    }
}
