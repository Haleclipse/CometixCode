//! Partial port of CC `utils/collapseReadSearch.ts`.
//! The canonical Read/Search collapse pass lives with its upstream utility owner;
//! `components::messages_list` remains only its Messages consumer.

use std::collections::{BTreeSet, HashMap};

use crate::types::message::{
    Attachment, CollapsedReadSearchEntry, CollapsedReadSearchGroup, RelevantMemory,
    RenderableMessage, RenderableMessageKind, StopHookInfo, SystemMessage, ToolResultStatus,
};
use crate::utils::memory_file_detection::{
    is_auto_managed_memory_file, is_auto_managed_memory_pattern, is_memory_directory,
    is_shell_command_targeting_memory,
};

// Utility-private equivalents of the ID/status projections used by
// `collapseReadSearch.ts#getToolUseIdsFromMessage` and
// `collapseReadSearch.ts#isCollapsibleToolResult`. Messages keeps its own
// lookup/grouping projections because those serve separate Messages passes.
/// The row's block is the first non-identity block — normalize
/// guarantees one real block per row.
fn assistant_tool_use_block(
    message: &RenderableMessage,
) -> Option<&crate::types::message::ToolUseBlock> {
    match &message.kind {
        RenderableMessageKind::Assistant { message } => match message.first_content_block() {
            Some(crate::types::message::AssistantContent::ToolUse(tool_use)) => Some(tool_use),
            _ => None,
        },
        _ => None,
    }
}

fn assistant_tool_use_id(message: &RenderableMessage) -> Option<&str> {
    assistant_tool_use_block(message)
        .map(|tool_use| tool_use.id.0.as_str())
        .filter(|id| !id.is_empty())
}

/// One block per row (normalize-guaranteed): the row's block is the first
/// content block.
fn user_tool_result_block(
    message: &RenderableMessage,
) -> Option<&crate::types::message::ToolResult> {
    match &message.kind {
        RenderableMessageKind::User { message } => match message.first_content_block() {
            Some(crate::types::message::UserContent::ToolResult(tool_result)) => Some(tool_result),
            _ => None,
        },
        _ => None,
    }
}

fn user_tool_result_id(message: &RenderableMessage) -> Option<&str> {
    user_tool_result_block(message)
        .map(|tool_result| tool_result.tool_use_id.0.as_str())
        .filter(|id| !id.is_empty())
}

/// `CollapsedReadSearchEntry` still stores a status until batch C; derive it
/// from the block exactly like the renderer does (cancel/reject prefixes, then
/// is_error — CC UserToolResultMessage.tsx).
fn derive_tool_result_status(tool_result: &crate::types::message::ToolResult) -> ToolResultStatus {
    if crate::utils::messages::is_tool_cancel_message(&tool_result.content) {
        ToolResultStatus::Canceled
    } else if crate::utils::messages::is_plain_tool_reject_message(&tool_result.content) {
        ToolResultStatus::Rejected
    } else if tool_result.is_error {
        ToolResultStatus::Error
    } else {
        ToolResultStatus::Success
    }
}

/// Maps to: CC `utils/collapseReadSearch.ts:71-76`
/// `getFilePathFromToolInput`.
fn get_file_path_from_tool_input(tool_input: Option<&serde_json::Value>) -> Option<&str> {
    let input = tool_input.and_then(serde_json::Value::as_object)?;
    input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(serde_json::Value::as_str)
}

/// Maps to: CC `utils/collapseReadSearch.ts:81-103` `isMemorySearch`.
fn is_memory_search(tool_input: Option<&serde_json::Value>) -> bool {
    let Some(input) = tool_input.and_then(serde_json::Value::as_object) else {
        return false;
    };
    if input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|path| is_auto_managed_memory_file(path) || is_memory_directory(path))
    {
        return true;
    }
    if input
        .get("glob")
        .and_then(serde_json::Value::as_str)
        .is_some_and(is_auto_managed_memory_pattern)
    {
        return true;
    }
    input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(is_shell_command_targeting_memory)
}

/// Maps to: CC `utils/collapseReadSearch.ts:109-116`
/// `isMemoryWriteOrEdit`.
fn is_memory_write_or_edit(tool_name: &str, tool_input: Option<&serde_json::Value>) -> bool {
    if tool_name != crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME
        && tool_name != crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME
    {
        return false;
    }
    get_file_path_from_tool_input(tool_input).is_some_and(is_auto_managed_memory_file)
}

/// Maps to: CC `utils/collapseReadSearch.ts:143-238` `getToolSearchOrReadInfo`
/// return shape. CC exports that function at module level and several callers
/// outside the collapse pass use it (`getSearchOrReadFromContent` :244-273,
/// `getCollapsibleToolInfo` :290-329, `AgentTool/UI.tsx:88`); this type and
/// the functions below therefore live at module level too.
pub(crate) struct ToolCollapseInfo {
    pub(crate) is_collapsible: bool,
    pub(crate) kind: &'static str,
    pub(crate) is_bash_command: bool,
    pub(crate) mcp_server_name: Option<String>,
}

impl ToolCollapseInfo {
    /// CC reads `info.isSearch` / `info.isRead` off the returned object; the
    /// Rust shape carries the same distinction in `kind`.
    pub(crate) fn is_search(&self) -> bool {
        self.kind == "search"
    }

    pub(crate) fn is_read(&self) -> bool {
        self.kind == "read"
    }

    pub(crate) fn is_repl(&self) -> bool {
        self.kind == "absorbed_silently"
    }
}

fn bash_collapsible_kind(command: &str) -> Option<&'static str> {
    // `collapseReadSearch.ts` consumes the Tool-owned
    // `isSearchOrReadCommand` metadata; keep parsing in BashTool.
    let kind = crate::tools::bash_tool::is_search_or_read_bash_command(command);
    if kind.is_list {
        Some("list")
    } else if kind.is_search {
        Some("search")
    } else if kind.is_read {
        Some("read")
    } else {
        None
    }
}

fn mcp_server_and_tool_name(tool_name: &str) -> Option<(String, String)> {
    // Maps to CC dynamic MCP tool names from `buildMcpToolName(...)`:
    // `mcp__<server>__<tool>`. Other display-only MCP labels are not
    // classified here because official collapse uses Tool.mcpInfo from the
    // dynamic tool definition, not transcript text heuristics.
    let trimmed = tool_name.trim();
    let (_, rest) = trimmed.split_once("__")?;
    let (server, tool) = rest.split_once("__")?;
    let server = server.trim();
    let tool = tool.trim();
    (!server.is_empty() && !tool.is_empty()).then(|| (server.to_string(), tool.to_string()))
}

fn mcp_server_name(tool_name: &str) -> Option<String> {
    let (server, tool) = mcp_server_and_tool_name(tool_name)?;
    let classification =
        crate::tools::mcp_tool::classify_for_collapse::classify_mcp_tool_for_collapse(
            &server, &tool,
        );
    (classification.is_search || classification.is_read).then_some(server)
}

/// Maps to: CC `utils/collapseReadSearch.ts:143-238`
/// `getToolSearchOrReadInfo`.
pub(crate) fn get_tool_search_or_read_info(
    tool_name: &str,
    input: Option<&serde_json::Value>,
) -> ToolCollapseInfo {
    let non_collapsible = || ToolCollapseInfo {
        is_collapsible: false,
        kind: "",
        is_bash_command: false,
        mcp_server_name: None,
    };
    if is_memory_write_or_edit(tool_name, input) {
        return ToolCollapseInfo {
            is_collapsible: true,
            kind: "memory_write",
            is_bash_command: false,
            mcp_server_name: None,
        };
    }
    let command = input
        .and_then(|input| input.get("command"))
        .and_then(serde_json::Value::as_str);
    match tool_name.to_ascii_lowercase().as_str() {
        "read" => ToolCollapseInfo {
            is_collapsible: true,
            kind: "read",
            is_bash_command: false,
            mcp_server_name: None,
        },
        "search" | "grep" | "glob" | "web search" => ToolCollapseInfo {
            is_collapsible: true,
            kind: "search",
            is_bash_command: false,
            mcp_server_name: None,
        },
        "list" | "ls" => ToolCollapseInfo {
            is_collapsible: true,
            kind: "list",
            is_bash_command: false,
            mcp_server_name: None,
        },
        "toolsearch" | "tool search" | "snip" | "repl" => ToolCollapseInfo {
            is_collapsible: true,
            kind: "absorbed_silently",
            is_bash_command: false,
            mcp_server_name: None,
        },
        "bash" => command
            .and_then(bash_collapsible_kind)
            .or_else(|| crate::utils::fullscreen::is_fullscreen_env_enabled().then_some("bash"))
            .map(|kind| ToolCollapseInfo {
                is_collapsible: true,
                kind,
                is_bash_command: true,
                mcp_server_name: None,
            })
            .unwrap_or_else(non_collapsible),
        "powershell" => command
            .and_then(|command| {
                let classification =
                    crate::tools::powershell_tool::is_search_or_read_powershell_command(command);
                if classification.is_search {
                    Some("search")
                } else if classification.is_read {
                    Some("read")
                } else {
                    None
                }
            })
            .map(|kind| ToolCollapseInfo {
                is_collapsible: true,
                kind,
                is_bash_command: true,
                mcp_server_name: None,
            })
            .unwrap_or_else(non_collapsible),
        _ => mcp_server_name(tool_name)
            .map(|server| ToolCollapseInfo {
                is_collapsible: true,
                kind: "mcp",
                is_bash_command: false,
                mcp_server_name: Some(server),
            })
            .unwrap_or_else(non_collapsible),
    }
}

/// Maps to: CC `utils/collapseReadSearch.ts:244-273`
/// `getSearchOrReadFromContent` — classify a tool_use block, returning `None`
/// unless it is collapsible or a REPL op (`:259` `info.isCollapsible ||
/// info.isREPL`). This is the entry `AgentTool/UI.tsx:88` calls.
pub(crate) fn get_search_or_read_from_content(
    tool_name: &str,
    input: Option<&serde_json::Value>,
) -> Option<ToolCollapseInfo> {
    let info = get_tool_search_or_read_info(tool_name, input);
    (info.is_collapsible || info.is_repl()).then_some(info)
}

/// Maps to: CC `utils/collapseReadSearch.ts:290-329`
/// `getCollapsibleToolInfo`.
fn get_collapsible_tool_info(message: &RenderableMessage) -> Option<ToolCollapseInfo> {
    let tool_use = assistant_tool_use_block(message)?;
    let input = (!tool_use.input.is_null()).then_some(&tool_use.input);
    let info = get_tool_search_or_read_info(tool_use.name.as_str(), input);
    info.is_collapsible.then_some(info)
}

/// Maps to: CC `utils/collapseReadSearch.ts:473-481`
/// `getToolUseIdsFromCollapsedGroup`. CC walks the group's raw `messages`;
/// Rust's group keeps the same tool uses as `verbose_entries`.
pub(crate) fn tool_use_ids_from_collapsed_group(
    group: &crate::types::message::CollapsedReadSearchGroup,
) -> impl Iterator<Item = &str> {
    group
        .verbose_entries
        .iter()
        .filter_map(|entry| match entry {
            CollapsedReadSearchEntry::ToolUse {
                tool_use_id: Some(tool_use_id),
                ..
            } => Some(tool_use_id.as_str()),
            _ => None,
        })
}

/// Maps to: CC `utils/collapseReadSearch.ts:486-494` `hasAnyToolInProgress`.
///
/// "Check if any tool in a collapsed group is in progress." The answer lives in
/// the REPL-owned set, not on the group — the collapse pass itself is a pure
/// message transform in CC and carries no liveness.
pub(crate) fn has_any_tool_in_progress(
    group: &crate::types::message::CollapsedReadSearchGroup,
    in_progress_tool_use_ids: &std::collections::HashSet<String>,
) -> bool {
    tool_use_ids_from_collapsed_group(group).any(|id| in_progress_tool_use_ids.contains(id))
}

/// Maps to: CC `utils/collapseReadSearch.ts:762-950` `collapseReadSearchGroups`.
/// The official pass collapses consecutive read/search/list tool calls and
/// their matching results into a single summary row, while deferring skippable
/// rows (thinking/system/attachments) until after the collapsed badge. This
/// Rust subset uses the already-normalized display tool names available at the
/// message-loading seam.
pub(crate) fn collapse_read_search_groups(
    messages: Vec<RenderableMessage>,
) -> Vec<RenderableMessage> {
    #[derive(Default)]
    struct ReadSearchGroup {
        first_uuid: String,
        tool_use_ids: BTreeSet<String>,
        read_paths: BTreeSet<String>,
        read_operation_count: usize,
        search_count: usize,
        list_count: usize,
        bash_count: usize,
        bash_commands: HashMap<String, String>,
        git_op_bash_count: usize,
        commits: Vec<crate::tools::shared::git_operation_tracking::GitCommitSummary>,
        pushes: Vec<crate::tools::shared::git_operation_tracking::GitPushSummary>,
        branches: Vec<crate::tools::shared::git_operation_tracking::GitBranchSummary>,
        prs: Vec<crate::tools::shared::git_operation_tracking::GitPrSummary>,
        mcp_call_count: usize,
        mcp_server_names: BTreeSet<String>,
        memory_search_count: usize,
        memory_read_paths: BTreeSet<String>,
        memory_read_operation_count: usize,
        memory_write_count: usize,
        team_memory_search_count: usize,
        team_memory_read_paths: BTreeSet<String>,
        team_memory_read_operation_count: usize,
        team_memory_write_count: usize,
        hook_total_ms: u64,
        hook_count: usize,
        hook_infos: Vec<StopHookInfo>,
        relevant_memories: Vec<RelevantMemory>,
        verbose_entries: Vec<CollapsedReadSearchEntry>,
        hint: String,
        errored: bool,
        deferred_skippable: Vec<RenderableMessage>,
    }

    const MAX_BASH_HINT_CHARS: usize = 300;

    /// Maps to: CC `utils/collapseReadSearch.ts:125-136` `commandAsHint`.
    fn command_as_hint(command: &str) -> String {
        let cleaned = format!(
            "$ {}",
            command
                .split('\n')
                .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        );
        if cleaned.chars().count() > MAX_BASH_HINT_CHARS {
            format!(
                "{}…",
                cleaned
                    .chars()
                    .take(MAX_BASH_HINT_CHARS - 1)
                    .collect::<String>()
            )
        } else {
            cleaned
        }
    }

    fn extract_quoted_tool_arg(description: &str, key: &str) -> Option<String> {
        let needle = format!("{key}: \"");
        let start = description.find(&needle)? + needle.len();
        let rest = &description[start..];
        let mut value = String::new();
        let mut escaped = false;
        for ch in rest.chars() {
            if escaped {
                value.push(ch);
                escaped = false;
                continue;
            }
            if ch == '\\' {
                value.push(ch);
                escaped = true;
                continue;
            }
            if ch == '"' {
                return Some(format!("\"{value}\""));
            }
            value.push(ch);
        }
        None
    }

    /// Maps to: CC `collapseReadSearch.ts`, which classifies collapsed rows from
    /// the tool's `param.input` rather than from rendered row text. Shell rows
    /// need the untruncated command, which `renderToolUseMessage` folds away.
    /// Rows recovered without a `ToolUseBlock` keep their rendered summary.
    fn collapse_command_text<'a>(
        tool_name: &str,
        input: Option<&'a serde_json::Value>,
        description: &'a str,
    ) -> &'a str {
        let key = match tool_name.to_ascii_lowercase().as_str() {
            "bash" | "powershell" => "command",
            "read" => "file_path",
            _ => return description,
        };
        input
            .and_then(|input| input.get(key))
            .and_then(|value| value.as_str())
            .unwrap_or(description)
    }

    fn tool_hint(tool_info: &ToolCollapseInfo, description: &str) -> String {
        if tool_info.is_bash_command {
            if tool_info.kind == "bash"
                && let Some(label) =
                    crate::tools::bash_tool::comment_label::extract_bash_comment_label(description)
            {
                return label;
            }
            return command_as_hint(description);
        }
        if tool_info.kind == "search" {
            if let Some(pattern) = extract_quoted_tool_arg(description, "pattern") {
                return pattern;
            }
            if let Some(glob) = extract_quoted_tool_arg(description, "glob") {
                return glob;
            }
        }
        description.to_string()
    }

    fn is_read_search_skippable(message: &RenderableMessage) -> bool {
        if let RenderableMessageKind::Assistant { message } = &message.kind {
            return matches!(
                message.first_content_block(),
                Some(
                    crate::types::message::AssistantContent::Thinking { .. }
                        | crate::types::message::AssistantContent::RedactedThinking { .. }
                )
            );
        }
        matches!(
            &message.kind,
            RenderableMessageKind::System(_) | RenderableMessageKind::Attachment(_)
        )
    }

    fn is_nested_memory_attachment(message: &RenderableMessage) -> bool {
        // Batch D2: typed `nested_memory` is exactly the old "Loaded …"
        // summary family; the Rust-only free-form `memory` tag is gone.
        matches!(
            &message.kind,
            RenderableMessageKind::Attachment(Attachment::NestedMemory { .. })
        )
    }

    fn pre_tool_hook_summary(
        message: &RenderableMessage,
    ) -> Option<(usize, u64, Vec<StopHookInfo>)> {
        let RenderableMessageKind::System(SystemMessage::StopHookSummary {
            hook_label: Some(label),
            hook_count,
            hook_infos,
            total_duration_ms,
            ..
        }) = &message.kind
        else {
            return None;
        };
        if label != "PreToolUse" {
            return None;
        }
        let effective_count = if *hook_count > 0 {
            *hook_count
        } else {
            hook_infos.len()
        };
        let total_ms = total_duration_ms.unwrap_or_else(|| {
            hook_infos
                .iter()
                .filter_map(|info| info.duration_ms)
                .sum::<u64>()
        });
        Some((effective_count, total_ms, hook_infos.clone()))
    }

    fn flush_group(result: &mut Vec<RenderableMessage>, group: &mut Option<ReadSearchGroup>) {
        let Some(group) = group.take() else {
            return;
        };
        result.push(RenderableMessage {
            uuid: format!("collapsed-{}", group.first_uuid),
            kind: RenderableMessageKind::CollapsedReadSearch(CollapsedReadSearchGroup {
                read_count: if group.read_paths.is_empty() {
                    group.read_operation_count
                } else {
                    group.read_paths.len()
                },
                search_count: group.search_count,
                list_count: group.list_count,
                bash_count: group.bash_count,
                git_op_bash_count: group.git_op_bash_count,
                commits: group.commits,
                pushes: group.pushes,
                branches: group.branches,
                prs: group.prs,
                mcp_call_count: group.mcp_call_count,
                mcp_server_names: group.mcp_server_names.into_iter().collect(),
                memory_search_count: group.memory_search_count,
                memory_read_count: group.memory_read_paths.len()
                    + group.memory_read_operation_count
                    + group.relevant_memories.len(),
                memory_write_count: group.memory_write_count,
                team_memory_search_count: group.team_memory_search_count,
                team_memory_read_count: group.team_memory_read_paths.len()
                    + group.team_memory_read_operation_count,
                team_memory_write_count: group.team_memory_write_count,
                hook_total_ms: (group.hook_count > 0).then_some(group.hook_total_ms),
                hook_count: group.hook_count,
                hook_infos: group.hook_infos,
                relevant_memories: group.relevant_memories,
                verbose_entries: group.verbose_entries,
                hint: group.hint,
                active: false,
                errored: group.errored,
            }),
        });
        result.extend(group.deferred_skippable);
    }

    let mut result = Vec::with_capacity(messages.len());
    let mut group: Option<ReadSearchGroup> = None;

    for message in messages {
        if let Some(tool_info) = get_collapsible_tool_info(&message) {
            let entry = group.get_or_insert_with(|| ReadSearchGroup {
                first_uuid: message.uuid.clone(),
                ..ReadSearchGroup::default()
            });
            let Some(tool_use) = assistant_tool_use_block(&message) else {
                result.push(message);
                continue;
            };
            let tool_name = &tool_use.name.clone();
            let tool_input_owned = tool_use.input.clone();
            let tool_input = (!tool_input_owned.is_null()).then_some(&tool_input_owned);
            match tool_info.kind {
                "read" => {
                    let file_path = tool_input
                        .and_then(serde_json::Value::as_object)
                        .and_then(|input| input.get("file_path"))
                        .and_then(serde_json::Value::as_str);
                    if let Some(file_path) = file_path {
                        #[cfg(feature = "anthropic_internal")]
                        let is_team_memory = crate::utils::team_memory_ops::is_team_mem_file(
                            std::path::Path::new(file_path),
                        );
                        #[cfg(not(feature = "anthropic_internal"))]
                        let is_team_memory = false;
                        if is_team_memory {
                            entry.team_memory_read_paths.insert(file_path.to_string());
                        } else if is_auto_managed_memory_file(file_path) {
                            entry.memory_read_paths.insert(file_path.to_string());
                        } else {
                            entry.read_paths.insert(file_path.to_string());
                        }
                    } else {
                        entry.read_operation_count += 1;
                    }
                }
                "search" => {
                    #[cfg(feature = "anthropic_internal")]
                    let is_team_memory =
                        crate::utils::team_memory_ops::is_team_memory_search(tool_input);
                    #[cfg(not(feature = "anthropic_internal"))]
                    let is_team_memory = false;
                    if is_team_memory {
                        entry.team_memory_search_count += 1;
                    } else if is_memory_search(tool_input) {
                        entry.memory_search_count += 1;
                    } else {
                        entry.search_count += 1;
                    }
                }
                "list" => entry.list_count += 1,
                "bash" => entry.bash_count += 1,
                "memory_write" => {
                    #[cfg(feature = "anthropic_internal")]
                    let is_team_memory =
                        crate::utils::team_memory_ops::is_team_memory_write_or_edit(
                            tool_name, tool_input,
                        );
                    #[cfg(not(feature = "anthropic_internal"))]
                    let is_team_memory = {
                        let _ = tool_name;
                        false
                    };
                    if is_team_memory {
                        entry.team_memory_write_count += 1;
                    } else {
                        entry.memory_write_count += 1;
                    }
                }
                "absorbed_silently" => {}
                "mcp" => {
                    entry.mcp_call_count += 1;
                    if let Some(server) = tool_info.mcp_server_name.as_ref() {
                        entry.mcp_server_names.insert(server.clone());
                    }
                }
                _ => {}
            }
            // The baked row description is gone; hint extraction still parses
            // the RENDERED summary (`tool_hint` pulls `pattern: "…"` out of
            // display text), so derive it here — render-time derivation at the
            // consumption site, same rule as the renderer itself.
            let rendered_description = tool_input
                .and_then(|input| {
                    crate::components::messages::assistant_tool_use_message::render_tool_use_message(
                        tool_name,
                        input,
                        crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
                    )
                })
                .unwrap_or_default();
            if let Some(tool_use_id) = assistant_tool_use_id(&message) {
                entry.tool_use_ids.insert(tool_use_id.to_string());
                if tool_info.kind == "bash" {
                    // `detect_git_operation` parses the command itself, so it
                    // needs the untruncated `param.input` command.
                    entry.bash_commands.insert(
                        tool_use_id.to_string(),
                        collapse_command_text(tool_name, tool_input, &rendered_description)
                            .to_string(),
                    );
                }
            }
            {
                entry
                    .verbose_entries
                    .push(CollapsedReadSearchEntry::ToolUse {
                        tool_name: tool_name.clone(),
                        input: tool_input.cloned(),
                        tool_use_id: assistant_tool_use_id(&message).map(str::to_string),
                        description: rendered_description.clone(),
                        // Unresolved until a matching tool result is seen. The
                        // loop below overwrites this from the arriving
                        // `ToolResult` (CC `CollapsedReadSearchContent.tsx:66-108`
                        // marks the loader from the resolved result), which is
                        // the same resolved/errored authority `lookups` carries
                        // — so the seed never needed the row's own status.
                        status: crate::types::message::ToolUseStatus::Queued,
                    });
                let hint_source =
                    collapse_command_text(tool_name, tool_input, &rendered_description);
                if tool_info.kind != "absorbed_silently" && !hint_source.trim().is_empty() {
                    entry.hint = tool_hint(&tool_info, hint_source);
                }
            }
            continue;
        }

        if let Some(active_group) = group.as_mut() {
            if let Some(tool_use_id) = user_tool_result_id(&message).map(str::to_string)
                && active_group.tool_use_ids.contains(tool_use_id.as_str())
            {
                if let Some(tool_result) = user_tool_result_block(&message) {
                    let status = derive_tool_result_status(tool_result);
                    // Maps to CC `CollapsedReadSearchContent.tsx:66-108`:
                    // the expanded row follows the tool use, marks its
                    // loader from the resolved result, and renders a result
                    // summary only for a non-error use with valid output.
                    // The tool name comes from the matched ToolUse entry —
                    // the result row does not store it.
                    let mut tool_name = String::new();
                    for verbose_entry in &mut active_group.verbose_entries {
                        if let CollapsedReadSearchEntry::ToolUse {
                            tool_use_id: Some(entry_id),
                            tool_name: entry_tool_name,
                            status: tool_status,
                            ..
                        } = verbose_entry
                            && *entry_id == tool_use_id
                        {
                            tool_name = entry_tool_name.clone();
                            *tool_status = if status == ToolResultStatus::Success {
                                crate::types::message::ToolUseStatus::Succeeded
                            } else {
                                crate::types::message::ToolUseStatus::Failed
                            };
                            break;
                        }
                    }
                    // Validity is the raw parsing under the tool's
                    // own output schema — CC's single rule for every tool
                    // (`CollapsedReadSearchContent.tsx:117-119` gates the
                    // result render on `outputSchema.safeParse` success);
                    // there is no display shape to probe for migrated
                    // tools (Read, Grep).
                    let valid_tool_output = if tool_name.eq_ignore_ascii_case("Read") {
                        tool_result.tool_use_result.as_ref().is_some_and(|raw| {
                            crate::tools::file_read_tool::parse_output(
                                &crate::tools::file_read_tool::javascript_runtime_value(raw),
                            )
                            .is_some()
                        })
                    } else if tool_name.eq_ignore_ascii_case("Grep") {
                        tool_result.tool_use_result.as_ref().is_some_and(|raw| {
                            crate::tools::grep_tool::ui::parse_output(raw).is_some()
                        })
                    } else if tool_name.eq_ignore_ascii_case("Glob") {
                        tool_result.tool_use_result.as_ref().is_some_and(|raw| {
                            crate::tools::glob_tool::ui::parse_output(raw).is_some()
                        })
                    } else {
                        true
                    };
                    if status == ToolResultStatus::Success && valid_tool_output {
                        active_group
                            .verbose_entries
                            .push(CollapsedReadSearchEntry::ToolResult {
                                tool_name,
                                status,
                                content: tool_result.content.clone(),
                                tool_use_result: tool_result.tool_use_result.clone(),
                            });
                    }
                    if status != ToolResultStatus::Success {
                        active_group.errored = true;
                    }
                }
                if let Some(command) = active_group.bash_commands.get(tool_use_id.as_str()) {
                    // Bash carries no display shape — read the raw
                    // `toolUseResult` with the tool's own output schema.
                    if let Some(output) = user_tool_result_block(&message)
                        .and_then(|tool_result| tool_result.tool_use_result.as_ref())
                        .and_then(crate::tools::bash_tool::ui::parse_output)
                    {
                        let output = format!("{}\n{}", output.stdout, output.stderr);
                        let detected =
                            crate::tools::shared::git_operation_tracking::detect_git_operation(
                                command, &output,
                            );
                        let mut found = false;
                        if let Some(commit) = detected.commit {
                            active_group.commits.push(commit);
                            found = true;
                        }
                        if let Some(push) = detected.push {
                            active_group.pushes.push(push);
                            found = true;
                        }
                        if let Some(branch) = detected.branch {
                            active_group.branches.push(branch);
                            found = true;
                        }
                        if let Some(pr) = detected.pr {
                            active_group.prs.push(pr);
                            found = true;
                        }
                        if found {
                            active_group.git_op_bash_count += 1;
                        }
                    }
                }
                continue;
            }

            if let Some((hook_count, hook_total_ms, hook_infos)) = pre_tool_hook_summary(&message) {
                active_group.hook_count += hook_count;
                active_group.hook_total_ms += hook_total_ms;
                active_group.hook_infos.extend(hook_infos);
                continue;
            }

            if let RenderableMessageKind::Attachment(Attachment::RelevantMemories { memories }) =
                &message.kind
            {
                active_group.relevant_memories.extend(memories.clone());
                continue;
            }

            if is_read_search_skippable(&message) {
                if is_nested_memory_attachment(&message) {
                    result.push(message);
                } else {
                    active_group.deferred_skippable.push(message);
                }
                continue;
            }
        }

        flush_group(&mut result, &mut group);
        result.push(message);
    }

    flush_group(&mut result, &mut group);
    result
}

/// Maps to: CC `collapseReadSearch.ts:966-973` `memoryCounts` — the optional
/// bag `getSearchReadSummaryText` accepts. The three team fields ride the same
/// struct in CC and only render under `feature('TEAMMEM')`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchReadMemoryCounts {
    pub memory_search_count: usize,
    pub memory_read_count: usize,
    pub memory_write_count: usize,
    pub team_memory_search_count: usize,
    pub team_memory_read_count: usize,
    pub team_memory_write_count: usize,
}

/// CC's repeated verb selector: present tense while active, past tense once
/// finished, and capitalised only when it opens the sentence
/// (`parts.length === 0`).
/// Rust-side de-duplication WITHIN this file only: CC inlines this ternary at
/// every call site. It is not a CC symbol, so it must not cross an owner
/// boundary — it was briefly `pub(crate)` for `team_memory_ops.rs`, which made
/// a helper with no upstream counterpart into shared API. That owner now
/// inlines its own ternaries, exactly as `teamMemoryOps.ts:54-84` does.
fn summary_verb<'a>(
    is_active: bool,
    is_first: bool,
    active: (&'a str, &'a str),
    done: (&'a str, &'a str),
) -> &'a str {
    let (upper, lower) = if is_active { active } else { done };
    if is_first { upper } else { lower }
}

/// Same scope rule as [`summary_verb`]: file-local only.
fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

/// Maps to: CC `collapseReadSearch.ts:961-1066#getSearchReadSummaryText`.
///
/// Part order is fixed: memory (recall → search → write) → team memory →
/// search → read → list → REPL. `is_active` selects present/past tense AND
/// appends the trailing `…`; capitalisation follows position, not category.
pub fn get_search_read_summary_text(
    search_count: usize,
    read_count: usize,
    is_active: bool,
    repl_count: usize,
    memory_counts: Option<&SearchReadMemoryCounts>,
    list_count: usize,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(counts) = memory_counts {
        if counts.memory_read_count > 0 {
            let verb = summary_verb(
                is_active,
                parts.is_empty(),
                ("Recalling", "recalling"),
                ("Recalled", "recalled"),
            );
            let count = counts.memory_read_count;
            parts.push(format!(
                "{verb} {count} {}",
                plural(count, "memory", "memories")
            ));
        }
        if counts.memory_search_count > 0 {
            let verb = summary_verb(
                is_active,
                parts.is_empty(),
                ("Searching", "searching"),
                ("Searched", "searched"),
            );
            parts.push(format!("{verb} memories"));
        }
        if counts.memory_write_count > 0 {
            let verb = summary_verb(
                is_active,
                parts.is_empty(),
                ("Writing", "writing"),
                ("Wrote", "wrote"),
            );
            let count = counts.memory_write_count;
            parts.push(format!(
                "{verb} {count} {}",
                plural(count, "memory", "memories")
            ));
        }
        // CC gates this on `feature('TEAMMEM')` (collapseReadSearch.ts:1017);
        // this file's other TEAMMEM branches use the same cfg boundary.
        #[cfg(feature = "anthropic_internal")]
        crate::utils::team_memory_ops::append_team_memory_summary_parts(
            counts, is_active, &mut parts,
        );
    }

    if search_count > 0 {
        let verb = summary_verb(
            is_active,
            parts.is_empty(),
            ("Searching for", "searching for"),
            ("Searched for", "searched for"),
        );
        parts.push(format!(
            "{verb} {search_count} {}",
            plural(search_count, "pattern", "patterns")
        ));
    }

    if read_count > 0 {
        let verb = summary_verb(
            is_active,
            parts.is_empty(),
            ("Reading", "reading"),
            ("Read", "read"),
        );
        parts.push(format!(
            "{verb} {read_count} {}",
            plural(read_count, "file", "files")
        ));
    }

    if list_count > 0 {
        let verb = summary_verb(
            is_active,
            parts.is_empty(),
            ("Listing", "listing"),
            ("Listed", "listed"),
        );
        parts.push(format!(
            "{verb} {list_count} {}",
            plural(list_count, "directory", "directories")
        ));
    }

    if repl_count > 0 {
        let verb = if is_active { "REPL'ing" } else { "REPL'd" };
        parts.push(format!(
            "{verb} {repl_count} {}",
            plural(repl_count, "time", "times")
        ));
    }

    let text = parts.join(", ");
    if is_active {
        format!("{text}…")
    } else {
        text
    }
}

/// Maps to: CC `collapseReadSearch.ts:1074-1109#summarizeRecentActivities` —
/// roll up TRAILING consecutive search/read activities (>= 2 of them), else
/// fall back to the most recent activity that carries a description (searching
/// backwards, because tools like SendMessage implement no
/// `getActivityDescription`).
pub fn summarize_recent_activities<A: RecentActivity>(activities: &[A]) -> Option<String> {
    if activities.is_empty() {
        return None;
    }
    let mut search_count = 0usize;
    let mut read_count = 0usize;
    for activity in activities.iter().rev() {
        if activity.is_search() {
            search_count += 1;
        } else if activity.is_read() {
            read_count += 1;
        } else {
            break;
        }
    }
    if search_count + read_count >= 2 {
        return Some(get_search_read_summary_text(
            search_count,
            read_count,
            true,
            0,
            None,
            0,
        ));
    }
    // CC `:1104` `if (activities[i]?.activityDescription)` is JS-truthy, so an
    // empty description is SKIPPED and the scan keeps walking backwards — the
    // filter belongs inside the search, not after it.
    activities.iter().rev().find_map(|activity| {
        activity
            .activity_description()
            .filter(|description| !description.is_empty())
    })
}

/// The structural shape CC's `summarizeRecentActivities` accepts
/// (`{activityDescription?, isSearch?, isRead?}`); Rust callers implement it on
/// their own activity row type.
pub trait RecentActivity {
    fn is_search(&self) -> bool;
    fn is_read(&self) -> bool;
    fn activity_description(&self) -> Option<String>;
}

#[cfg(test)]
mod summary_text_tests {
    use super::*;

    /// CC `collapseReadSearch.ts:1021-1066`: present tense while active, past
    /// tense once finished, capitalised ONLY when the part opens the sentence,
    /// and the trailing `…` rides `isActive`.
    #[test]
    fn summary_text_matches_official_tense_case_and_ellipsis() {
        assert_eq!(
            get_search_read_summary_text(2, 0, true, 0, None, 0),
            "Searching for 2 patterns…"
        );
        assert_eq!(
            get_search_read_summary_text(1, 0, false, 0, None, 0),
            "Searched for 1 pattern"
        );
        // Later parts lowercase; order is search → read → list → REPL.
        assert_eq!(
            get_search_read_summary_text(1, 3, true, 2, None, 1),
            "Searching for 1 pattern, reading 3 files, listing 1 directory, REPL'ing 2 times…"
        );
        assert_eq!(
            get_search_read_summary_text(0, 2, false, 1, None, 2),
            "Read 2 files, listed 2 directories, REPL'd 1 time"
        );
        // No parts: CC still appends the ellipsis while active (joining an
        // empty list yields the empty string).
        assert_eq!(get_search_read_summary_text(0, 0, true, 0, None, 0), "…");
        assert_eq!(get_search_read_summary_text(0, 0, false, 0, None, 0), "");
    }

    /// CC `:979-1013`: memory parts come FIRST, in recall → search → write
    /// order, taking the leading capital away from the search/read parts.
    #[test]
    fn memory_parts_lead_the_summary_like_official() {
        let counts = SearchReadMemoryCounts {
            memory_search_count: 1,
            memory_read_count: 2,
            memory_write_count: 1,
            ..SearchReadMemoryCounts::default()
        };
        assert_eq!(
            get_search_read_summary_text(1, 0, true, 0, Some(&counts), 0),
            "Recalling 2 memories, searching memories, writing 1 memory, searching for 1 pattern…"
        );
        assert_eq!(
            get_search_read_summary_text(0, 1, false, 0, Some(&counts), 0),
            "Recalled 2 memories, searched memories, wrote 1 memory, read 1 file"
        );
    }

    struct Activity {
        description: Option<&'static str>,
        is_search: bool,
        is_read: bool,
    }

    impl RecentActivity for Activity {
        fn is_search(&self) -> bool {
            self.is_search
        }

        fn is_read(&self) -> bool {
            self.is_read
        }

        fn activity_description(&self) -> Option<String> {
            self.description.map(ToOwned::to_owned)
        }
    }

    fn activity(description: Option<&'static str>, is_search: bool, is_read: bool) -> Activity {
        Activity {
            description,
            is_search,
            is_read,
        }
    }

    /// CC `:1074-1109`: only TRAILING consecutive search/read activities roll
    /// up, the rollup needs at least two of them, and the fallback searches
    /// BACKWARDS for the most recent description (tools like SendMessage
    /// implement no `getActivityDescription`).
    #[test]
    fn recent_activity_rollup_matches_official_trailing_window_and_fallback() {
        assert_eq!(summarize_recent_activities::<Activity>(&[]), None);

        // Two trailing collapsibles roll up; the earlier non-collapsible stops
        // the backwards scan and contributes nothing.
        let activities = [
            activity(Some("Running tests"), false, false),
            activity(None, true, false),
            activity(None, false, true),
        ];
        assert_eq!(
            summarize_recent_activities(&activities).as_deref(),
            Some("Searching for 1 pattern, reading 1 file…")
        );

        // A single trailing collapsible is below the threshold — fall back.
        let activities = [
            activity(Some("Running tests"), false, false),
            activity(None, false, true),
        ];
        assert_eq!(
            summarize_recent_activities(&activities).as_deref(),
            Some("Running tests")
        );

        // The fallback skips description-less rows, walking backwards.
        let activities = [
            activity(Some("Earlier work"), false, false),
            activity(Some("Latest work"), false, false),
            activity(None, false, false),
        ];
        assert_eq!(
            summarize_recent_activities(&activities).as_deref(),
            Some("Latest work")
        );

        // Nothing to roll up and no descriptions at all.
        let activities = [activity(None, false, false)];
        assert_eq!(summarize_recent_activities(&activities), None);

        // CC `:1104` is JS-truthy: an EMPTY description is skipped and the
        // backwards scan continues, rather than ending the search.
        let activities = [
            activity(Some("Earlier work"), false, false),
            activity(Some(""), false, false),
        ];
        assert_eq!(
            summarize_recent_activities(&activities).as_deref(),
            Some("Earlier work")
        );
    }
}
