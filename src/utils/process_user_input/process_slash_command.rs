//! Slash command processing seam.
//! Maps to official `utils/processUserInput/processSlashCommand.tsx`: resolve a
//! submitted `/command`, apply command metadata/enabled gates, and return a
//! typed local command UI action plus transcript messages/`should_query`. The
//! Rust port keeps UI side effects out of this module; `screens::repl` executes
//! the returned [`SlashCommandAction`] so REPL remains the UI state owner.

use super::ProcessUserInputBaseResult;
use crate::commands::{self, Command, CommandKind};
use crate::constants::product;
use crate::constants::query_source::QuerySource;
use crate::services::mcp::client::McpPromptCommandSnapshot;
use crate::types::message::{
    RenderableMessage, RenderableMessageKind, SystemMessage, SystemMessageLevel, UserContent,
};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalSettingsTab {
    Status,
    Config,
    Usage,
}

/// Rust name for official CC local-jsx slash command UI payloads.
///
/// The official implementation stores React JSX in `toolJSX`; Cometix stores a
/// typed Rust command UI descriptor instead.
#[derive(Clone, Debug, PartialEq)]
pub enum LocalCommandUi {
    /// Maps to CC commands/plugin/plugin.tsx's returned PluginSettings JSX.
    PluginSettings {
        data: crate::commands::plugin::plugin::PluginSettingsData,
        preceding_input_blocks: Vec<UserContent>,
    },
    AddDir {
        args: String,
    },
    Tasks {
        context: std::sync::Arc<crate::tool::ToolUseContext>,
    },
    Settings {
        default_tab: LocalSettingsTab,
    },
    ResumePicker,
    Help,
    Hooks,
    Model,
    Fast,
    Theme,
    Export {
        data: crate::commands::export::export::ExportDialogData,
        preceding_input_blocks: Vec<UserContent>,
    },
    Permissions,
    Memory,
    Agents,
    Skills,
    Stats,
    Doctor,
    Diff,
    Sandbox,
    /// Maps to: CC `/btw` local-jsx side-question panel.
    Btw {
        question: String,
        /// Maps to CC `BtwComponentProps.context`: the exact context object
        /// captured when the immediate command is submitted.
        context: std::sync::Arc<crate::tool::ToolUseContext>,
    },
    Copy {
        data: crate::commands::copy::CopyPickerData,
    },
    Mcp {
        args: String,
    },
    /// Maps to CC `commands/login/login.tsx` local JSX flow.
    Login {
        args: String,
    },
    /// Maps to CC `commands/logout/logout.tsx` local JSX flow.
    Logout {
        args: String,
    },
    /// Maps to CC `commands/ide/ide.tsx` local JSX flow.
    Ide {
        args: String,
    },
    /// Maps to: CC `/effort` empty-args EffortPicker (`Zsm`).
    Effort {
        args: Option<String>,
        has_conversation_messages: bool,
    },
}

/// Canonical invocation metadata for a resolved slash command.
///
/// This intentionally stays separate from the command descriptor registry and
/// from the UI payload: official command objects provide metadata such as name
/// and aliases, while the invocation carries the canonical command name and the
/// submitted args used for command/output transcript rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommandInvocation {
    pub command_name: String,
    pub args: String,
}

impl SlashCommandInvocation {
    pub fn new(command_name: impl Into<String>, args: impl Into<String>) -> Self {
        Self {
            command_name: command_name.into(),
            args: args.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SlashCommandAction {
    /// Carries the local request declared by CC `commands/advisor.ts`.
    /// The business callback remains owned by `commands/advisor/advisor.rs`;
    /// this carrier retains its input envelope until async validation finishes.
    Advisor {
        args: String,
        context: std::sync::Arc<crate::tool::ToolUseContext>,
        invocation: SlashCommandInvocation,
        user_message: crate::types::message::UserMessage,
    },
    /// Maps to CC commands/reload-plugins/reload-plugins.ts async local call.
    /// A3/A6 retained carrier; processSlashCommand owns its text/error rows.
    ReloadPlugins {
        invocation: SlashCommandInvocation,
        user_message: crate::types::message::UserMessage,
    },
    /// Maps to CC commands/export/export.tsx call promise.
    ExportConversation {
        preceding_input_blocks: Vec<UserContent>,
        args: String,
        context: std::sync::Arc<crate::tool::ToolUseContext>,
        invocation: SlashCommandInvocation,
    },
    /// Maps to: CC `commands/terminalSetup/terminalSetup.tsx:174-222` call.
    /// The synchronous dispatcher defers installer I/O to the existing REPL
    /// async callback carrier (PORTING.md A3/A6).
    SetupTerminal {
        invocation: SlashCommandInvocation,
    },
    BranchConversation {
        args: String,
        invocation: SlashCommandInvocation,
    },
    SetSessionColor {
        args: String,
        context: std::sync::Arc<crate::tool::ToolUseContext>,
        invocation: SlashCommandInvocation,
    },
    OpenLocalCommandUi {
        command: LocalCommandUi,
        invocation: SlashCommandInvocation,
    },
    ResumeByArg {
        arg: String,
        invocation: SlashCommandInvocation,
    },
    /// REPL owns OSC 52 output; the command module owns content selection and
    /// temp-file fallback formatting.
    CopyToClipboard {
        content: crate::commands::copy::CopyContent,
        invocation: SlashCommandInvocation,
    },
    /// Empty `/rename` awaits the small-model request outside the synchronous
    /// command dispatcher while retaining the original invocation/history.
    GenerateSessionName {
        request: crate::commands::rename::RenameGenerationRequest,
        invocation: SlashCommandInvocation,
    },
    /// `/context` analysis performs memory/config discovery and static ANSI
    /// rendering off the retained update frame, then completes through the
    /// same local-command `onDone(output)` transcript path as CC.
    AnalyzeContext {
        request: crate::commands::context::ContextCommandRequest,
        invocation: SlashCommandInvocation,
    },
    /// Carries the in-memory context prepared by `/plan` into QueryParams when
    /// the command description requests an immediate model turn.
    ApplyPlanMode {
        next_context: crate::tool::ToolPermissionContext,
    },
    /// Reads/renders the current plan (and optionally opens its editor) outside
    /// the retained submit callback.
    InspectPlan {
        request: crate::commands::plan::plan::PlanInspectionRequest,
        invocation: SlashCommandInvocation,
    },
    /// Runs the model-backed compact pipeline off the retained frame and then
    /// installs the returned compact boundary/history atomically in REPL.
    CompactConversation {
        custom_instructions: String,
        context: std::sync::Arc<crate::tool::ToolUseContext>,
        invocation: SlashCommandInvocation,
    },
    /// Creates/preserves keybindings.json, hands terminal ownership to the
    /// external editor, then reloads the live keybinding runtime off-frame.
    EditKeybindings {
        invocation: SlashCommandInvocation,
    },
    /// REPL-owned, UI-only in-memory clear. Mirrors official `/clear` local
    /// command without invoking sessionStorage writes or hook execution.
    ClearConversation,
    Exit,
    /// Maps to: CC `commands/rewind/rewind.ts` — `context.openMessageSelector()`
    /// then `{type: 'skip'}` (no messages appended).
    OpenMessageSelector,
}

fn next_id(uuid: Option<String>) -> String {
    uuid.unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn warning_result(uuid: Option<String>, text: impl Into<String>) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        messages: vec![RenderableMessage::system_notice(
            next_id(uuid),
            text,
            SystemMessageLevel::Warning,
        )],
        should_query: false,
        allowed_tools: None,
        local_action: None,
        query_source: QuerySource::Prompt,
    }
}

fn command_message(
    id: String,
    command: impl Into<String>,
    args: impl Into<String>,
) -> RenderableMessage {
    // The row carries the wire text; UserTextMessage's
    // `<command-message>` tag dispatch renders the command breadcrumb.
    RenderableMessage::user(id, format_command_input_tags(&command.into(), &args.into()))
}

/// Maps to: CC `utils/messages.ts:577-583` `formatCommandInputTags` — the
/// command-input breadcrumb the model sees when a slash command runs. The
/// template-string indentation is preserved verbatim on the wire, and
/// `<command-args>` is always present (even when empty).
fn format_command_input_tags(command: &str, args: &str) -> String {
    use crate::constants::xml::{COMMAND_ARGS_TAG, COMMAND_MESSAGE_TAG, COMMAND_NAME_TAG};

    format!(
        "<{COMMAND_NAME_TAG}>/{command}</{COMMAND_NAME_TAG}>\n            <{COMMAND_MESSAGE_TAG}>{command}</{COMMAND_MESSAGE_TAG}>\n            <{COMMAND_ARGS_TAG}>{args}</{COMMAND_ARGS_TAG}>"
    )
}

fn prompt_command_message(id: String, command: &str, args: &str) -> RenderableMessage {
    RenderableMessage::user(id, format_command_input_tags(command, args))
}

/// Builds the transcript/query result for a resolved MCP prompt slash command.
/// Maps to CC `utils/processUserInput/processSlashCommand.tsx`
/// `processPromptSlashCommand(...)` returning command metadata plus prompt
/// content blocks for the model turn.
pub fn mcp_prompt_slash_command_result(
    uuid: Option<String>,
    command: &McpPromptCommandSnapshot,
    submitted_args: &str,
    content_blocks: &[Value],
) -> ProcessUserInputBaseResult {
    let command_id = next_id(uuid);
    let prompt_id = Uuid::new_v4().to_string();
    ProcessUserInputBaseResult {
        messages: vec![
            prompt_command_message(command_id, &command.name, submitted_args),
            RenderableMessage::user_block(
                prompt_id,
                crate::types::message::UserContent::MetaText(
                    crate::services::mcp::client::mcp_prompt_content_blocks_summary(content_blocks),
                ),
            ),
        ],
        should_query: true,
        allowed_tools: None,
        local_action: None,
        query_source: QuerySource::Prompt,
    }
}

/// Builds the transcript result for an MCP prompt slash-command error.
/// Maps to CC `processSlashCommand.tsx` prompt-command catch branch, which
/// returns the formatted command input plus `<local-command-stderr>` and does
/// not query the model.
pub fn mcp_prompt_slash_command_error_result(
    uuid: Option<String>,
    command: &McpPromptCommandSnapshot,
    submitted_args: &str,
    error: &str,
) -> ProcessUserInputBaseResult {
    let command_id = next_id(uuid);
    let output_id = Uuid::new_v4().to_string();
    ProcessUserInputBaseResult {
        messages: vec![
            command_message(command_id, command.name.clone(), submitted_args.to_string()),
            local_command_output_message(output_id, error.to_string(), true),
        ],
        should_query: false,
        allowed_tools: None,
        local_action: None,
        query_source: QuerySource::Prompt,
    }
}

/// Maps to: CC `processSlashCommand.tsx` `formatSkillLoadingMetadata(...)`.
pub(crate) fn format_skill_loading_metadata(skill_name: &str, _progress_message: &str) -> String {
    format!(
        "<command-message>{skill_name}</command-message>\n<command-name>{skill_name}</command-name>\n<skill-format>true</skill-format>"
    )
}

/// Maps to: CC `processSlashCommand.tsx:1049-1060`.
fn format_slash_command_loading_metadata(command_name: &str, args: &str) -> String {
    let mut metadata = format!(
        "<command-message>{command_name}</command-message>\n<command-name>/{command_name}</command-name>"
    );
    if !args.is_empty() {
        metadata.push_str(&format!("\n<command-args>{args}</command-args>"));
    }
    metadata
}

/// Maps to: CC `processSlashCommand.tsx:1067-1086`.
fn format_command_loading_metadata(command: &Command, args: &str) -> String {
    use crate::skills::load_skills_dir::SkillLoadedFrom;
    if !command.user_invocable
        && matches!(
            command.loaded_from,
            Some(SkillLoadedFrom::Skills | SkillLoadedFrom::Plugin | SkillLoadedFrom::Mcp)
        )
    {
        return format_skill_loading_metadata(
            &command.name,
            command.progress_message.as_deref().unwrap_or("loading"),
        );
    }
    format_slash_command_loading_metadata(&command.name, args)
}

/// Maps to: CC `processSlashCommand.tsx:1089-1112#processPromptSlashCommand`.
pub(crate) fn process_prompt_slash_command(
    command_name: &str,
    args: &str,
    registry: &[Command],
    context: &crate::tool::ToolUseContext,
    image_content_blocks: Vec<UserContent>,
) -> anyhow::Result<ProcessUserInputBaseResult> {
    // Maps to: CC processSlashCommand.tsx:1089-1112. Tool invocation uses
    // the same prompt owner without the user slash-dispatch enabled gate.
    let command = commands::find_command(command_name, registry)
        .ok_or_else(|| anyhow::anyhow!("Unknown command: {command_name}"))?;
    if command.kind != CommandKind::Prompt {
        let kind = match command.kind {
            CommandKind::Local => "local",
            CommandKind::LocalUi => "local-jsx",
            CommandKind::Prompt => unreachable!(),
        };
        anyhow::bail!(
            "Unexpected {kind} command. Expected 'prompt' command. Use /{command_name} directly in the main conversation."
        );
    }
    get_messages_for_prompt_slash_command(command, args, context, image_content_blocks, None)
}

/// Maps to: CC `processSlashCommand.tsx:1114-1263#getMessagesForPromptSlashCommand`.
fn get_messages_for_prompt_slash_command(
    command: &Command,
    args: &str,
    context: &crate::tool::ToolUseContext,
    image_content_blocks: Vec<UserContent>,
    uuid: Option<String>,
) -> anyhow::Result<ProcessUserInputBaseResult> {
    // Maps to CC `getMessagesForSlashCommand`: record user-invocable prompt
    // skill usage before materializing the prompt so ranking observes the
    // invocation even when prompt expansion later fails.
    if command.user_invocable {
        crate::utils::suggestions::skill_usage_tracking::record_skill_usage(
            commands::get_command_name(command),
        );
    }
    let Some(get_prompt) = command.get_prompt_for_command else {
        anyhow::bail!(
            "Command /{} is registered as a prompt slash command, but its execution is not wired.",
            commands::get_command_name(command)
        );
    };
    let prompt = get_prompt(command, args, context)?;
    // CC registers permitted frontmatter hooks before recording invocation.
    if let Some(skill) = &command.prompt_command {
        skill.register_skill_hooks(&crate::bootstrap::state::get_session_id());
    }
    let source = match command.source {
        commands::CommandSource::Builtin => "builtin",
        commands::CommandSource::PolicySettings => "policySettings",
        commands::CommandSource::UserSettings => "userSettings",
        commands::CommandSource::ProjectSettings => "projectSettings",
        commands::CommandSource::Plugin => "plugin",
        commands::CommandSource::Bundled => "bundled",
        commands::CommandSource::Mcp => "mcp",
    };
    let skill_content = prompt
        .iter()
        .filter_map(|block| match block {
            UserContent::Text(text) | UserContent::MetaText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    crate::bootstrap::state::add_invoked_skill(
        command.name.as_ref(),
        format!("{source}:{}", command.name),
        skill_content,
        context.agent_id.as_deref(),
    );
    let allowed_tools = crate::utils::permissions::permission_setup::parse_tool_list_from_cli(
        &command.allowed_tools,
    );
    // CC puts images before prompt content under one isMeta envelope. Meta*
    // variants are the established Rust carrier at both UI and wire boundaries.
    // precedingInputBlocks has no independent carrier in current submit params.
    let content = image_content_blocks
        .into_iter()
        .chain(prompt)
        .map(|block| match block {
            UserContent::Text(text) => UserContent::MetaText(text),
            UserContent::Image { media_type, data } => UserContent::MetaImage { media_type, data },
            UserContent::RawImage { block, .. } => UserContent::RawImage {
                block,
                is_meta: true,
            },
            UserContent::Document { media_type, data } => {
                UserContent::MetaDocument { media_type, data }
            }
            block => block,
        })
        .collect();
    // Existing partial source boundary: CC :1215-1244 extracts attachments
    // from expanded prompt blocks via getAttachmentMessages(skipSkillDiscovery)
    // before command_permissions. That extraction is not wired here yet;
    // sharing this owner with Skill does not supply @file/MCP/agent attachments.
    Ok(ProcessUserInputBaseResult {
        messages: vec![
            RenderableMessage::user(
                next_id(uuid),
                format_command_loading_metadata(command, args),
            ),
            RenderableMessage::user_blocks(Uuid::new_v4().to_string(), content),
            RenderableMessage {
                uuid: Uuid::new_v4().to_string(),
                kind: RenderableMessageKind::Attachment(
                    crate::utils::attachments::Attachment::CommandPermissions {
                        allowed_tools: allowed_tools.clone(),
                        model: command
                            .prompt_command
                            .as_ref()
                            .and_then(|prompt| prompt.model.clone()),
                    },
                ),
            },
        ],
        should_query: true,
        allowed_tools: Some(allowed_tools),
        local_action: None,
        query_source: QuerySource::Prompt,
    })
}

fn local_command_output_message(
    id: String,
    output: impl Into<String>,
    is_error: bool,
) -> RenderableMessage {
    use crate::constants::xml::{LOCAL_COMMAND_STDERR_TAG, LOCAL_COMMAND_STDOUT_TAG};
    // The row carries the wire text (CC utils/messages.ts:598 shape);
    // UserTextMessage's `<local-command-std*>` dispatch renders it.
    let output = output.into();
    let text = if is_error {
        format!("<{LOCAL_COMMAND_STDERR_TAG}>{output}</{LOCAL_COMMAND_STDERR_TAG}>")
    } else {
        format!("<{LOCAL_COMMAND_STDOUT_TAG}>{output}</{LOCAL_COMMAND_STDOUT_TAG}>")
    };
    RenderableMessage::user(id, text)
}

/// Rust projection of local-JSX `onDone(result, { display: 'system' })`.
/// Maps to CC `processSlashCommand.tsx:741-798` plus
/// `utils/messages.ts:576-582` `formatCommandInputTags`.
pub(crate) fn system_display_local_command_result(
    uuid: Option<String>,
    command: &str,
    args: &str,
    output: &str,
) -> ProcessUserInputBaseResult {
    use crate::constants::xml::{
        COMMAND_ARGS_TAG, COMMAND_MESSAGE_TAG, COMMAND_NAME_TAG, LOCAL_COMMAND_STDOUT_TAG,
    };

    // CC processSlashCommand.tsx:769-784: fullscreen suppresses modal
    // dismissal transcript rows, while ordinary system output remains visible.
    // This carrier has no metaMessages; callers with meta data retain it in
    // their source-owned branch.
    if crate::utils::fullscreen::is_fullscreen_env_enabled() && output.ends_with(" dismissed") {
        return ProcessUserInputBaseResult {
            messages: Vec::new(),
            should_query: false,
            allowed_tools: None,
            local_action: None,
            query_source: QuerySource::Prompt,
        };
    }

    let input = format!(
        "<{COMMAND_NAME_TAG}>/{command}</{COMMAND_NAME_TAG}>\n            <{COMMAND_MESSAGE_TAG}>{command}</{COMMAND_MESSAGE_TAG}>\n            <{COMMAND_ARGS_TAG}>{args}</{COMMAND_ARGS_TAG}>"
    );
    ProcessUserInputBaseResult {
        messages: vec![
            RenderableMessage {
                uuid: next_id(uuid),
                kind: RenderableMessageKind::System(SystemMessage::local_command(input)),
            },
            RenderableMessage {
                uuid: Uuid::new_v4().to_string(),
                kind: RenderableMessageKind::System(SystemMessage::local_command(format!(
                    "<{LOCAL_COMMAND_STDOUT_TAG}>{output}</{LOCAL_COMMAND_STDOUT_TAG}>"
                ))),
            },
        ],
        should_query: false,
        allowed_tools: None,
        local_action: None,
        query_source: QuerySource::Prompt,
    }
}

fn local_command_result(
    uuid: Option<String>,
    command: impl Into<String>,
    args: impl Into<String>,
    output: impl Into<String>,
    is_error: bool,
    local_action: Option<SlashCommandAction>,
) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        messages: vec![
            command_message(next_id(uuid), command, args),
            local_command_output_message(Uuid::new_v4().to_string(), output, is_error),
        ],
        should_query: false,
        allowed_tools: None,
        local_action,
        query_source: QuerySource::Prompt,
    }
}

pub(crate) fn model_visible_local_command_result(
    uuid: Option<String>,
    command: &str,
    args: &str,
    output: &str,
    should_query: bool,
    local_action: Option<SlashCommandAction>,
) -> ProcessUserInputBaseResult {
    // CC processSlashCommand.tsx:779-806: both onDone branches send the
    // formatCommandInputTags breadcrumb (NOT a bare `/command`) followed by
    // the `<local-command-stdout>` wrapped result.
    ProcessUserInputBaseResult {
        messages: vec![
            RenderableMessage::user(next_id(uuid), format_command_input_tags(command, args)),
            local_command_output_message(Uuid::new_v4().to_string(), output.to_string(), false),
        ],
        should_query,
        allowed_tools: None,
        local_action,
        query_source: QuerySource::Prompt,
    }
}

/// Maps to: CC `processSlashCommand.tsx:860-866` before awaiting local call.
/// A3/A6: retain this envelope across the await instead of changing the
/// input UUID/timestamp when the potentially slow command finishes.
pub(crate) fn local_text_command_input(
    invocation: &SlashCommandInvocation,
) -> crate::types::message::UserMessage {
    crate::utils::messages::create_user_message(format_command_input_tags(
        &invocation.command_name,
        &invocation.args,
    ))
}

/// Maps to: CC `processSlashCommand.tsx:859-947` local text result/catch.
/// A3/A6: completion of a deferred local call retains its preceding blocks;
/// unlike local-JSX display:'system', its input row remains a user message.
pub(crate) fn local_text_command_result(
    user_message: crate::types::message::UserMessage,
    output: Result<String, String>,
) -> ProcessUserInputBaseResult {
    let (tag, output) = match output {
        Ok(output) => (crate::constants::xml::LOCAL_COMMAND_STDOUT_TAG, output),
        Err(error) => (crate::constants::xml::LOCAL_COMMAND_STDERR_TAG, error),
    };
    let mut result = ProcessUserInputBaseResult {
        messages: vec![
            RenderableMessage {
                uuid: user_message.uuid.clone(),
                kind: RenderableMessageKind::User {
                    message: user_message,
                },
            },
            RenderableMessage {
                uuid: Uuid::new_v4().to_string(),
                kind: RenderableMessageKind::System(SystemMessage::local_command(format!(
                    "<{tag}>{output}</{tag}>"
                ))),
            },
        ],
        should_query: false,
        allowed_tools: None,
        local_action: None,
        query_source: QuerySource::Prompt,
    };
    prepend_local_command_caveat(&mut result);
    result
}

/// Maps to: CC `processSlashCommand.tsx:735-813` local-JSX `onDone`.
/// Reuses the canonical message/tag constructors; the native UI continuation
/// calls this after its component callback, before the ordinary hook tail.
pub(crate) fn local_jsx_command_result(
    command: &str,
    args: &str,
    output: Option<&str>,
    display: Option<crate::utils::worktree::CommandResultDisplay>,
    should_query: bool,
    meta_messages: &[String],
    preceding_input_blocks: Vec<UserContent>,
) -> ProcessUserInputBaseResult {
    use crate::utils::worktree::CommandResultDisplay;
    if display == Some(CommandResultDisplay::Skip) {
        return ProcessUserInputBaseResult {
            messages: Vec::new(),
            should_query: false,
            allowed_tools: None,
            local_action: None,
            query_source: QuerySource::Prompt,
        };
    }
    let mut result = if display == Some(CommandResultDisplay::System) {
        // JavaScript interpolation preserves undefined for explicit system
        // output, whereas the default user branch uses NO_CONTENT_MESSAGE.
        system_display_local_command_result(None, command, args, output.unwrap_or("undefined"))
    } else {
        model_visible_local_command_result(
            None,
            command,
            args,
            output
                .filter(|text| !text.is_empty())
                .unwrap_or(crate::utils::messages::NO_CONTENT_MESSAGE),
            should_query,
            None,
        )
    };
    if display != Some(CommandResultDisplay::System) && !preceding_input_blocks.is_empty() {
        if let Some(RenderableMessage {
            kind: RenderableMessageKind::User { message },
            ..
        }) = result.messages.first_mut()
        {
            message.content = crate::utils::messages::prepare_user_content(
                format_command_input_tags(command, args),
                preceding_input_blocks,
            );
        }
    }
    result.should_query = should_query;
    result.messages.extend(meta_messages.iter().map(|text| {
        let message = crate::utils::messages::create_user_message_with_meta(text.clone(), true);
        RenderableMessage {
            uuid: message.uuid.clone(),
            kind: RenderableMessageKind::User { message },
        }
    }));
    prepend_local_command_caveat(&mut result);
    result
}

pub(crate) fn open_local_command_ui(
    command: LocalCommandUi,
    invocation: SlashCommandInvocation,
) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        local_action: Some(SlashCommandAction::OpenLocalCommandUi {
            command,
            invocation,
        }),
        query_source: QuerySource::Prompt,
    }
}

fn format_cost_summary() -> String {
    // Maps to CC `cost-tracker.ts::formatTotalCost`; unlike the old preview,
    // every value comes from the live/restored process cost state.
    let cost = crate::cost_tracker::get_total_cost();
    let cost = if cost > 0.5 {
        format!("${cost:.2}")
    } else {
        format!("${cost:.4}")
    };
    let input = crate::cost_tracker::get_total_input_tokens();
    let output = crate::cost_tracker::get_total_output_tokens();
    let model_usage = crate::cost_tracker::get_model_usage();
    let cache_read = model_usage
        .values()
        .map(|usage| usage.cache_read_input_tokens)
        .sum::<u64>();
    let cache_write = model_usage
        .values()
        .map(|usage| usage.cache_creation_input_tokens)
        .sum::<u64>();
    let lines_added = crate::cost_tracker::get_total_lines_added();
    let lines_removed = crate::cost_tracker::get_total_lines_removed();

    [
        format!("Total cost:            {cost}"),
        format!(
            "Total duration (API):  {}",
            crate::utils::format::format_duration(
                crate::cost_tracker::get_total_api_duration()
            )
        ),
        format!(
            "Total duration (wall): {}",
            crate::utils::format::format_duration(crate::cost_tracker::get_total_duration())
        ),
        format!(
            "Total code changes:    {lines_added} {} added, {lines_removed} {} removed",
            if lines_added == 1 { "line" } else { "lines" },
            if lines_removed == 1 { "line" } else { "lines" },
        ),
        format!(
            "Usage:                 {input} input, {output} output, {cache_read} cache read, {cache_write} cache write"
        ),
    ]
    .join("\n")
}

fn invocation(command: &Command, args: &str) -> SlashCommandInvocation {
    SlashCommandInvocation::new(commands::get_command_name(command), args)
}

macro_rules! define_local_ui_call {
    ($name:ident, $command:expr) => {
        pub(crate) fn $name(
            command: &Command,
            args: &str,
            _uuid: Option<String>,
            _context: &crate::tool::ToolUseContext,
        ) -> ProcessUserInputBaseResult {
            open_local_command_ui($command, invocation(command, args))
        }
    };
}

define_local_ui_call!(
    call_config,
    LocalCommandUi::Settings {
        default_tab: LocalSettingsTab::Config
    }
);
define_local_ui_call!(
    call_status,
    LocalCommandUi::Settings {
        default_tab: LocalSettingsTab::Status
    }
);
define_local_ui_call!(
    call_usage,
    LocalCommandUi::Settings {
        default_tab: LocalSettingsTab::Usage
    }
);
define_local_ui_call!(call_help, LocalCommandUi::Help);
define_local_ui_call!(call_hooks, LocalCommandUi::Hooks);
define_local_ui_call!(call_fast, LocalCommandUi::Fast);
define_local_ui_call!(call_theme, LocalCommandUi::Theme);
pub(crate) fn call_export(
    command: &Command,
    args: &str,
    _uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        local_action: Some(SlashCommandAction::ExportConversation {
            preceding_input_blocks: Vec::new(),
            args: args.to_string(),
            context: std::sync::Arc::new(context.clone()),
            invocation: invocation(command, args),
        }),
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        query_source: QuerySource::Prompt,
    }
}
define_local_ui_call!(call_permissions, LocalCommandUi::Permissions);
define_local_ui_call!(call_memory, LocalCommandUi::Memory);
define_local_ui_call!(call_agents, LocalCommandUi::Agents);
define_local_ui_call!(call_skills, LocalCommandUi::Skills);
define_local_ui_call!(call_doctor, LocalCommandUi::Doctor);
define_local_ui_call!(call_diff, LocalCommandUi::Diff);

pub(crate) fn call_resume(
    command: &Command,
    args: &str,
    _uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    let invocation = invocation(command, args);
    if args.is_empty() {
        open_local_command_ui(LocalCommandUi::ResumePicker, invocation)
    } else {
        ProcessUserInputBaseResult {
            messages: Vec::new(),
            should_query: false,
            allowed_tools: None,
            local_action: Some(SlashCommandAction::ResumeByArg {
                arg: args.to_string(),
                invocation,
            }),
            query_source: QuerySource::Prompt,
        }
    }
}

pub(crate) fn call_compact(
    command: &Command,
    args: &str,
    _uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        local_action: Some(SlashCommandAction::CompactConversation {
            custom_instructions: args.trim().to_string(),
            context: std::sync::Arc::new(context.clone()),
            invocation: invocation(command, args),
        }),
        query_source: QuerySource::Prompt,
    }
}

pub(crate) fn call_clear(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    local_command_result(
        uuid,
        commands::get_command_name(command),
        args,
        "",
        false,
        Some(SlashCommandAction::ClearConversation),
    )
}

pub(crate) fn call_version(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    local_command_result(
        uuid,
        commands::get_command_name(command),
        args,
        product::VERSION,
        false,
        None,
    )
}

pub(crate) fn call_cost(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    local_command_result(
        uuid,
        commands::get_command_name(command),
        args,
        format_cost_summary(),
        false,
        None,
    )
}

/// Maps to: CC `commands/context/context.tsx#call`.
pub(crate) fn call_context(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    let _ = uuid;
    let invocation = invocation(command, args);
    ProcessUserInputBaseResult {
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        local_action: Some(SlashCommandAction::AnalyzeContext {
            request: commands::context::ContextCommandRequest::new(context),
            invocation,
        }),
        query_source: QuerySource::Prompt,
    }
}

/// Maps to: CC `commands/context/context-noninteractive.ts#call`.
pub(crate) fn call_context_noninteractive(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    let request = commands::context::ContextCommandRequest::new(context);
    local_command_result(
        uuid,
        commands::get_command_name(command),
        args,
        commands::context::context_noninteractive::call(&request),
        false,
        None,
    )
}

/// Maps to: CC `commands/plan/plan.tsx#call`.
pub(crate) fn call_plan(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    match commands::plan::plan::call(args, context) {
        commands::plan::plan::PlanCall::Enabled {
            output,
            should_query,
            next_context,
        } => model_visible_local_command_result(
            uuid,
            commands::get_command_name(command),
            args,
            &output,
            should_query,
            should_query.then_some(SlashCommandAction::ApplyPlanMode { next_context }),
        ),
        commands::plan::plan::PlanCall::Inspect(request) => ProcessUserInputBaseResult {
            messages: Vec::new(),
            should_query: false,
            allowed_tools: None,
            local_action: Some(SlashCommandAction::InspectPlan {
                request,
                invocation: invocation(command, args),
            }),
            query_source: QuerySource::Prompt,
        },
    }
}

pub(crate) fn call_model(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    if let Some(output) = commands::model::inline_output_for_args(args) {
        local_command_result(
            uuid,
            commands::get_command_name(command),
            args,
            output,
            false,
            None,
        )
    } else {
        open_local_command_ui(LocalCommandUi::Model, invocation(command, args))
    }
}

pub(crate) fn call_sandbox(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    if args.is_empty() {
        open_local_command_ui(LocalCommandUi::Sandbox, invocation(command, args))
    } else {
        let output = commands::sandbox::local_output_for_args(args);
        let is_error = commands::sandbox::local_output_is_error(&output);
        local_command_result(
            uuid,
            commands::get_command_name(command),
            args,
            output,
            is_error,
            None,
        )
    }
}

/// Maps to: CC `commands/effort/effort.tsx#call` (`Zsm`).
pub(crate) fn call_effort(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    match commands::effort::effort::call(args, context) {
        commands::effort::effort::EffortCall::OpenPicker => open_local_command_ui(
            LocalCommandUi::Effort {
                args: None,
                has_conversation_messages: !context.messages.is_empty(),
            },
            invocation(command, args),
        ),
        commands::effort::effort::EffortCall::ConfirmArgs(value) => open_local_command_ui(
            LocalCommandUi::Effort {
                args: Some(value),
                has_conversation_messages: !context.messages.is_empty(),
            },
            invocation(command, args),
        ),
        commands::effort::effort::EffortCall::Result(result) => local_command_result(
            uuid,
            commands::get_command_name(command),
            args,
            result.message,
            false,
            None,
        ),
    }
}

/// Maps to: CC `commands/vim/vim.ts:8-38::call`.
pub(crate) fn call_vim(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    match commands::vim::call() {
        Ok(change) => local_command_result(
            uuid,
            commands::get_command_name(command),
            args,
            change.output,
            false,
            None,
        ),
        Err(error) => local_command_result(
            uuid,
            commands::get_command_name(command),
            args,
            format!("Failed to change editor mode: {error}"),
            true,
            None,
        ),
    }
}

pub(crate) fn call_terminal_setup(
    command: &Command,
    args: &str,
    _uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        local_action: Some(SlashCommandAction::SetupTerminal {
            invocation: invocation(command, args),
        }),
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        query_source: QuerySource::Prompt,
    }
}

pub(crate) fn call_mcp(
    command: &Command,
    args: &str,
    _uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    open_local_command_ui(
        LocalCommandUi::Mcp {
            args: args.to_string(),
        },
        invocation(command, args),
    )
}

/// Maps to: CC `commands/copy/copy.tsx::call`.
pub(crate) fn call_copy(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    match crate::commands::copy::copy::call(&context.messages, args) {
        crate::commands::copy::copy::CopyCall::Output { text, is_error } => local_command_result(
            uuid,
            commands::get_command_name(command),
            args,
            text,
            is_error,
            None,
        ),
        crate::commands::copy::copy::CopyCall::Direct(content) => ProcessUserInputBaseResult {
            messages: Vec::new(),
            should_query: false,
            allowed_tools: None,
            local_action: Some(SlashCommandAction::CopyToClipboard {
                content,
                invocation: invocation(command, args),
            }),
            query_source: QuerySource::Prompt,
        },
        crate::commands::copy::copy::CopyCall::Picker(data) => {
            open_local_command_ui(LocalCommandUi::Copy { data }, invocation(command, args))
        }
    }
}

/// Maps to: CC `commands/rename/rename.ts::call`.
pub(crate) fn call_rename(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    match crate::commands::rename::rename::call(context, args) {
        // CC rename.ts:27-30, 41-44, 82: each normal onDone requests
        // display:'system', including teammate rejection and no-context output.
        crate::commands::rename::RenameCall::Output {
            text,
            is_error: false,
        } => system_display_local_command_result(
            uuid,
            &commands::get_command_name(command),
            args,
            &text,
        ),
        crate::commands::rename::RenameCall::Output {
            text,
            is_error: true,
        } => {
            // CC processSlashCommand.tsx:844-858 catches failed local-JSX
            // calls, logs them and releases dispatch with no transcript rows.
            crate::utils::log::log_error(crate::utils::log::LogError::new(text));
            ProcessUserInputBaseResult {
                messages: Vec::new(),
                should_query: false,
                allowed_tools: None,
                local_action: None,
                query_source: QuerySource::Prompt,
            }
        }
        crate::commands::rename::RenameCall::Generate(request) => ProcessUserInputBaseResult {
            messages: Vec::new(),
            should_query: false,
            allowed_tools: None,
            local_action: Some(SlashCommandAction::GenerateSessionName {
                request,
                invocation: invocation(command, args),
            }),
            query_source: QuerySource::Prompt,
        },
    }
}

pub(crate) fn call_rewind(
    _command: &Command,
    _args: &str,
    _uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        local_action: Some(SlashCommandAction::OpenMessageSelector),
        query_source: QuerySource::Prompt,
    }
}

pub(crate) fn call_exit(
    _command: &Command,
    _args: &str,
    _uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    ProcessUserInputBaseResult {
        messages: Vec::new(),
        should_query: false,
        allowed_tools: None,
        local_action: Some(SlashCommandAction::Exit),
        query_source: QuerySource::Prompt,
    }
}

/// Maps to: CC `commands/init.ts:239-253::getPromptForCommand` through the
/// generic prompt-command transport in `processSlashCommand.tsx`.
pub(crate) fn call_init(
    _command: &Command,
    _args: &str,
    _context: &crate::tool::ToolUseContext,
) -> anyhow::Result<Vec<UserContent>> {
    Ok(vec![UserContent::Text(
        commands::init::get_prompt_for_command().to_string(),
    )])
}

pub(crate) fn call_skill_prompt(
    command: &Command,
    args: &str,
    context: &crate::tool::ToolUseContext,
) -> anyhow::Result<Vec<UserContent>> {
    let Some(skill) = command.prompt_command.as_ref() else {
        anyhow::bail!(
            "Command /{} has no loaded prompt implementation.",
            commands::get_command_name(command)
        );
    };
    let session_id = crate::bootstrap::state::get_session_id();
    Ok(vec![UserContent::Text(skill.get_prompt_for_command(
        Some(args),
        &session_id,
        context,
    )?)])
}

pub fn process_slash_command(
    input: &str,
    uuid: Option<String>,
    registry: &[Command],
) -> ProcessUserInputBaseResult {
    process_slash_command_with_context(
        input,
        uuid,
        registry,
        &crate::tool::ToolUseContext::default(),
        Vec::new(),
    )
}

pub fn process_slash_command_with_context(
    input: &str,
    uuid: Option<String>,
    registry: &[Command],
    context: &crate::tool::ToolUseContext,
    image_content_blocks: Vec<UserContent>,
) -> ProcessUserInputBaseResult {
    let mut result =
        process_slash_command_inner(input, uuid, registry, context, image_content_blocks);
    prepend_local_command_caveat(&mut result);
    result
}

/// Maps to: CC `processSlashCommand.tsx:675-681` — the caveat is prepended at
/// the function's exits, under three exemptions:
///
/// ```text
/// messageShouldQuery || newMessages.every(isSystemLocalCommandMessage) || isCompactResult
///   ? newMessages
///   : [createSyntheticUserCaveatMessage(), ...newMessages]
/// ```
///
/// - `shouldQuery`: the command's output IS the turn's prompt, so it is not
///   "output the user produced on the side" — it is what they are asking.
/// - all-`local_command` system rows: UI-only, filtered before the API, so
///   there is nothing for the model to misread.
/// - a compact result: `isCompactResult` checks the FIRST message
///   (:670-673); compact "handles its own synthetic caveat ordering".
///
/// CC's other exits (:406, :448, :606) prepend unconditionally, and all three
/// satisfy these predicates anyway — `shouldQuery: false` with a non-system
/// first message — so one exit point reproduces every case.
pub(crate) fn prepend_local_command_caveat(result: &mut ProcessUserInputBaseResult) {
    let all_local_command_rows = result
        .messages
        .iter()
        .all(crate::utils::messages::is_system_local_command_message);
    let is_compact_result = result
        .messages
        .first()
        .is_some_and(crate::utils::messages::is_compact_boundary_row);
    if result.should_query || all_local_command_rows || is_compact_result {
        return;
    }
    let caveat = crate::utils::messages::create_synthetic_user_caveat_message();
    result.messages.insert(
        0,
        RenderableMessage {
            uuid: caveat.uuid.clone(),
            kind: RenderableMessageKind::User { message: caveat },
        },
    );
}

fn process_slash_command_inner(
    input: &str,
    uuid: Option<String>,
    registry: &[Command],
    context: &crate::tool::ToolUseContext,
    image_content_blocks: Vec<UserContent>,
) -> ProcessUserInputBaseResult {
    let Some(parsed) = crate::utils::slash_command_parsing::parse_slash_command(input) else {
        return warning_result(uuid, "Commands are in the form `/command [args]`");
    };

    if parsed.name.is_empty() {
        return warning_result(uuid, "Commands are in the form `/command [args]`");
    }

    // CC processSlashCommand.tsx:420-426 passes the exact parsed name to hasCommand.
    let registry_name = parsed.name.as_str();
    let Some(command) = commands::find_command(registry_name, registry) else {
        return warning_result(uuid, format!("Unknown command: /{}", parsed.name));
    };

    if !commands::is_command_enabled(command) {
        return warning_result(
            uuid,
            format!(
                "Command /{} is currently disabled.",
                commands::get_command_name(command)
            ),
        );
    }

    // CC :507-510 forwards parsedArgs without a second trim.
    let args = parsed.args.as_str();
    if command.kind == CommandKind::Prompt {
        return get_messages_for_prompt_slash_command(
            command,
            args,
            context,
            image_content_blocks,
            uuid.clone(),
        )
        .unwrap_or_else(|error| {
            local_command_result(
                uuid,
                commands::get_command_name(command),
                args,
                error.to_string(),
                true,
                None,
            )
        });
    }
    if let Some(call) = command.call {
        let mut result = call(command, args, uuid, context);
        match result.local_action.as_mut() {
            Some(SlashCommandAction::Advisor { user_message, .. }) => {
                user_message.content = crate::utils::messages::prepare_user_content(
                    format_command_input_tags(&parsed.name, args),
                    image_content_blocks,
                );
            }
            Some(SlashCommandAction::ReloadPlugins {
                invocation,
                user_message,
            }) => {
                user_message.content = crate::utils::messages::prepare_user_content(
                    format_command_input_tags(&invocation.command_name, &invocation.args),
                    image_content_blocks,
                );
            }
            Some(SlashCommandAction::ExportConversation {
                preceding_input_blocks,
                ..
            })
            | Some(SlashCommandAction::OpenLocalCommandUi {
                command:
                    LocalCommandUi::PluginSettings {
                        preceding_input_blocks,
                        ..
                    },
                ..
            }) => {
                *preceding_input_blocks = image_content_blocks;
            }
            _ => {}
        }
        return result;
    }

    let kind = match command.kind {
        CommandKind::Prompt => "prompt slash command",
        CommandKind::Local => "local command",
        CommandKind::LocalUi => "local command UI",
    };
    warning_result(
        uuid,
        format!(
            "Command /{} is registered as a {kind}, but its execution is not wired.",
            commands::get_command_name(command)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::declared_commands_for_tests;
    use crate::utils::test_env::{EnvVarGuard, PinnedProjectDir, TEST_ENV_LOCK};

    fn get_commands() -> &'static [Command] {
        static COMMANDS: std::sync::LazyLock<Vec<Command>> =
            std::sync::LazyLock::new(declared_commands_for_tests);
        &COMMANDS
    }

    /// CC prepends `createSyntheticUserCaveatMessage()` at the slash-command
    /// exits (processSlashCommand.tsx:606,681), so a command's own rows start
    /// after it. Tests address those rows, not the envelope.
    fn rows_after_caveat(result: &ProcessUserInputBaseResult) -> &[RenderableMessage] {
        let is_caveat = result.messages.first().is_some_and(|row| {
            matches!(
                &row.kind,
                RenderableMessageKind::User { message }
                    if matches!(
                        message.first_content_block(),
                        Some(crate::types::message::UserContent::MetaText(text))
                            if text.starts_with(&format!(
                                "<{}>",
                                crate::constants::xml::LOCAL_COMMAND_CAVEAT_TAG
                            ))
                    )
            )
        });
        if is_caveat {
            &result.messages[1..]
        } else {
            &result.messages
        }
    }

    fn warning_text(result: &ProcessUserInputBaseResult) -> &str {
        match &rows_after_caveat(result)[0].kind {
            RenderableMessageKind::System(SystemMessage::Informational { content, .. }) => content,
            other => panic!("expected system warning, got {other:?}"),
        }
    }

    /// User rows carry the wire text; local command output is the full
    /// `<local-command-stdout>`/`<local-command-stderr>` string
    /// (CC utils/messages.ts:598 shape). Returns it verbatim so tests pin the
    /// wire shape, tags included.
    fn local_output(result: &ProcessUserInputBaseResult) -> &str {
        user_text(&rows_after_caveat(result)[1])
    }

    /// Reads the row's Text block the way the renderer does
    /// (CC `Message.tsx:284` text branch).
    fn user_text(message: &RenderableMessage) -> &str {
        match &message.kind {
            RenderableMessageKind::User { message } => match message.first_content_block() {
                Some(crate::types::message::UserContent::Text(text)) => text,
                other => panic!("expected user text block, got {other:?}"),
            },
            other => panic!("expected user row, got {other:?}"),
        }
    }

    /// Exercises real descriptors through the same dispatcher used by REPL.
    /// Every result is local data; even /review only constructs its prompt here.
    #[test]
    fn tui_completed_commands_reach_registered_actions_and_prompt_without_model_io() {
        let context = crate::tool::ToolUseContext::default();
        for input in ["/tasks", "/bashes"] {
            let result = process_slash_command_with_context(
                input,
                None,
                get_commands(),
                &context,
                Vec::new(),
            );
            assert!(!result.should_query);
            assert!(result.messages.is_empty());
            assert!(
                matches!(result.local_action,
                    Some(SlashCommandAction::OpenLocalCommandUi {
                        command: LocalCommandUi::Tasks { context: actual }, invocation,
                    }) if *actual == context && invocation == SlashCommandInvocation::new("tasks", "")
                ),
                "{input}"
            );
        }

        let add_dir = process_slash_command("/add-dir  /tmp/extra", None, get_commands());
        assert!(!add_dir.should_query);
        assert!(add_dir.messages.is_empty());
        assert_eq!(
            add_dir.local_action,
            Some(SlashCommandAction::OpenLocalCommandUi {
                command: LocalCommandUi::AddDir {
                    args: " /tmp/extra".to_string()
                },
                invocation: SlashCommandInvocation::new("add-dir", " /tmp/extra"),
            })
        );

        for (input, expected_args) in [
            ("/color green", "green"),
            ("\u{feff}/color \u{85}green\u{feff}", "\u{85}green"),
            ("/color   GREEN", "  GREEN"),
        ] {
            let result = process_slash_command_with_context(
                input,
                None,
                get_commands(),
                &context,
                Vec::new(),
            );
            assert!(!result.should_query);
            assert!(result.messages.is_empty());
            assert!(
                matches!(result.local_action,
                    Some(SlashCommandAction::SetSessionColor { args, context: actual, invocation })
                        if args == expected_args && *actual == context
                            && invocation == SlashCommandInvocation::new("color", expected_args)
                ),
                "{input:?}"
            );
        }

        let output_style = commands::find_command("output-style", get_commands()).unwrap();
        assert!(output_style.is_hidden);
        let result = process_slash_command("/output-style", None, get_commands());
        assert!(!result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(result.messages.len(), 2);
        assert!(
            result
                .messages
                .iter()
                .all(crate::utils::messages::is_system_local_command_message)
        );

        let review = process_slash_command("/review 42", None, get_commands());
        assert!(review.should_query);
        assert!(review.local_action.is_none());
        assert!(
            review.messages.iter().any(|row| match &row.kind {
                RenderableMessageKind::User { message } =>
                    message.content.iter().any(|block| matches!(
                        block, UserContent::Text(text) | UserContent::MetaText(text)
                            if text == &crate::commands::review::local_review_prompt("42")
                    )),
                _ => false,
            }),
            "registered review must produce its real local prompt"
        );
    }

    #[test]
    fn dispatch_preserves_official_case_sensitive_command_matching() {
        // CC commands.ts:688-700 compares name/display name/aliases exactly.
        let mut command = crate::commands::color::command();
        command.name = "MiXeD".into();
        let registry = vec![command];
        let accepted = process_slash_command("/MiXeD  green", None, &registry);
        assert!(matches!(accepted.local_action,
            Some(SlashCommandAction::SetSessionColor { args, invocation, .. })
                if args == " green" && invocation.command_name == "MiXeD"
        ));
        let rejected = process_slash_command("/mixed green", None, &registry);
        assert!(rejected.local_action.is_none());
        assert!(!rejected.should_query);
    }

    #[test]
    fn prompt_messages_matches_official_allowed_tools_transport() {
        let command = Command::from_skill(crate::skills::load_skills_dir::SkillCommand {
            name: "review".to_string(),
            display_name: None,
            description: "Review".to_string(),
            has_user_specified_description: true,
            markdown_content: "Review the code".to_string(),
            content_length: 15,
            allowed_tools: vec!["Read(/repo/**)".to_string(), "Grep".to_string()],
            argument_hint: None,
            argument_names: Vec::new(),
            when_to_use: None,
            version: None,
            model: None,
            effort: None,
            disable_model_invocation: false,
            user_invocable: true,
            execution_context: crate::skills::load_skills_dir::SkillExecutionContext::Inline,
            agent: None,
            shell: None,
            hooks: None,
            paths: None,
            source: crate::skills::load_skills_dir::SkillSource::ProjectSettings,
            loaded_from: crate::skills::load_skills_dir::SkillLoadedFrom::Skills,
            skill_root: None,
            file_path: std::path::PathBuf::from("SKILL.md"),
        });
        assert_eq!(
            command.allowed_tools,
            command.prompt_command.as_ref().unwrap().allowed_tools
        );

        let result = get_messages_for_prompt_slash_command(
            &command,
            "",
            &crate::tool::ToolUseContext::default(),
            Vec::new(),
            None,
        )
        .unwrap();

        assert_eq!(
            result.allowed_tools,
            Some(vec!["Read(/repo/**)".to_string(), "Grep".to_string()])
        );
        // CC processSlashCommand.tsx:1247-1251: the same rules travel on the
        // persisted command_permissions attachment, including empty lists.
        assert!(matches!(&result.messages[2].kind,
            RenderableMessageKind::Attachment(crate::utils::attachments::Attachment::CommandPermissions {
                allowed_tools, model: None,
            }) if allowed_tools == &vec!["Read(/repo/**)".to_string(), "Grep".to_string()]
        ));
    }

    #[test]
    fn builtin_prompt_matches_official_command_allowed_tools_transport() {
        let mut command = crate::commands::statusline::command();
        // The generic command owns these grants even without a captured skill.
        assert!(command.prompt_command.is_none());
        for (declared, expected) in [
            (
                vec!["Agent", "Read(~/**)", "Edit(~/.claude/settings.json)"],
                vec!["Agent", "Read(~/**)", "Edit(~/.claude/settings.json)"],
            ),
            (
                vec!["Read,Grep", "Bash(echo hi)"],
                vec!["Read", "Grep", "Bash(echo hi)"],
            ),
            (vec![], vec![]),
        ] {
            command.allowed_tools = declared.into_iter().map(str::to_string).collect();
            let expected = expected.into_iter().map(str::to_string).collect::<Vec<_>>();
            let result = process_slash_command_with_context(
                "/statusline use model and cwd",
                Some("statusline-invocation".into()),
                &[command.clone()],
                &crate::tool::ToolUseContext::default(),
                Vec::new(),
            );
            assert!(result.should_query);
            assert!(result.local_action.is_none());
            assert_eq!(result.messages.len(), 3);
            assert_eq!(result.messages[0].uuid, "statusline-invocation");
            assert!(matches!(&result.messages[0].kind,
                RenderableMessageKind::User { message }
                    if message.content == vec![UserContent::Text(
                        "<command-message>statusline</command-message>\n<command-name>/statusline</command-name>\n<command-args>use model and cwd</command-args>".into()
                    )]
            ));
            assert!(matches!(&result.messages[1].kind,
                RenderableMessageKind::User { message }
                    if message.content == vec![UserContent::MetaText(
                        "Create an Agent with subagent_type \"statusline-setup\" and the prompt \"use model and cwd\"".into()
                    )]
            ));
            assert_eq!(result.allowed_tools, Some(expected.clone()));
            assert!(matches!(&result.messages[2].kind,
                RenderableMessageKind::Attachment(crate::utils::attachments::Attachment::CommandPermissions {
                    allowed_tools, model: None,
                }) if allowed_tools == &expected
            ));
        }
    }

    #[test]
    fn prompt_failure_matches_official_no_invocation_or_meta_payload() {
        // CC processSlashCommand.tsx:1170 throws before :1172-1253's hooks,
        // invocation, image/meta message and command_permissions construction.
        let agent = "failed-prompt-command-agent";
        crate::bootstrap::state::clear_invoked_skills_for_agent(agent);
        let mut command = commands::find_command("init", get_commands())
            .unwrap()
            .clone();
        command.get_prompt_for_command = Some(|_, _, _| anyhow::bail!("fixture prompt failure"));
        let mut context = crate::tool::ToolUseContext::default();
        context.agent_id = Some(agent.to_string());
        let failure =
            process_prompt_slash_command("init", "", &[command.clone()], &context, Vec::new())
                .unwrap_err();
        assert_eq!(failure.to_string(), "fixture prompt failure");
        let result = process_slash_command_with_context(
            "/init",
            None,
            &[command],
            &context,
            vec![UserContent::Image {
                media_type: "image/png".into(),
                data: "ignored".into(),
            }],
        );
        assert!(!result.should_query);
        assert!(local_output(&result).contains("fixture prompt failure"));
        assert!(crate::bootstrap::state::get_invoked_skills_for_agent(Some(agent)).is_empty());
        let rows = rows_after_caveat(&result);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(&row.kind, RenderableMessageKind::Attachment(_)))
        );
        assert!(!rows.iter().any(|row| matches!(&row.kind,
            RenderableMessageKind::User { message }
                if message.content.iter().any(|block| !matches!(block, UserContent::Text(_)))
        )));
    }

    #[test]
    fn prompt_callback_matches_official_generic_entry_without_user_dispatch_gate() {
        let mut command = commands::find_command("init", get_commands())
            .unwrap()
            .clone();
        command.name = "callback-probe".into();
        command.is_enabled = Some(|| false);
        command.get_prompt_for_command = Some(|_, args, _| {
            Ok(vec![UserContent::Text(format!(
                "Loaded {args}; preserve <command-message> in prompt content"
            ))])
        });
        let mut context = crate::tool::ToolUseContext::default();
        context.agent_id = Some(format!("callback-probe-{}", Uuid::new_v4()));
        let result = process_prompt_slash_command(
            "callback-probe",
            "payload",
            &[command],
            &context,
            Vec::new(),
        )
        .unwrap();
        assert!(result.should_query);
        assert_eq!(result.messages.len(), 3);
        assert_eq!(
            user_text(&result.messages[0]),
            "<command-message>callback-probe</command-message>\n<command-name>/callback-probe</command-name>\n<command-args>payload</command-args>"
        );
        assert!(
            matches!(&result.messages[1].kind, RenderableMessageKind::User { message }
            if matches!(message.content.as_slice(), [UserContent::MetaText(text)]
                if text == "Loaded payload; preserve <command-message> in prompt content"))
        );
        assert_eq!(
            crate::bootstrap::state::get_invoked_skills_for_agent(context.agent_id.as_deref())
                .len(),
            1
        );
        crate::bootstrap::state::clear_invoked_skills_for_agent(
            context.agent_id.as_deref().unwrap(),
        );
    }

    #[test]
    fn mcp_prompt_slash_command_result_matches_official_command_plus_prompt_shape() {
        let command = McpPromptCommandSnapshot {
            name: "mcp__docs__summarize".to_string(),
            description: "Summarize docs".to_string(),
            has_user_specified_description: true,
            user_facing_name: "docs:summarize (MCP)".to_string(),
            arg_names: vec!["path".to_string()],
            source: "mcp",
        };
        let result = mcp_prompt_slash_command_result(
            Some("cmd-1".to_string()),
            &command,
            "README.md",
            &[
                serde_json::json!({"type":"text","text":"Read README.md"}),
                serde_json::json!({"type":"text","text":"Summarize key points"}),
            ],
        );

        assert!(result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(result.messages.len(), 2);
        // The command breadcrumb is CC formatCommandInputTags verbatim
        // (utils/messages.ts:577-583) — template indentation preserved.
        assert_eq!(
            user_text(&result.messages[0]),
            "<command-name>/mcp__docs__summarize</command-name>\n            <command-message>mcp__docs__summarize</command-message>\n            <command-args>README.md</command-args>"
        );
        assert!(matches!(
            &result.messages[1].kind,
            RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(crate::types::message::UserContent::MetaText(text))
                    if text == "Read README.md\nSummarize key points"
            )
        ));
    }

    #[test]
    fn mcp_prompt_slash_command_error_result_matches_official_stderr_shape() {
        let command = McpPromptCommandSnapshot {
            name: "mcp__docs__summarize".to_string(),
            description: "Summarize docs".to_string(),
            has_user_specified_description: true,
            user_facing_name: "docs:summarize (MCP)".to_string(),
            arg_names: vec!["path".to_string()],
            source: "mcp",
        };
        let result = mcp_prompt_slash_command_error_result(
            Some("cmd-1".to_string()),
            &command,
            "README.md",
            "Error: prompt failed",
        );

        assert!(!result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(result.messages.len(), 2);
        assert_eq!(
            user_text(&result.messages[0]),
            "<command-name>/mcp__docs__summarize</command-name>\n            <command-message>mcp__docs__summarize</command-message>\n            <command-args>README.md</command-args>"
        );
        // `is_error` routes to the stderr tag on the wire.
        assert_eq!(
            local_output(&result),
            "<local-command-stderr>Error: prompt failed</local-command-stderr>"
        );
    }

    #[test]
    fn process_slash_command_recognizes_local_command_ui() {
        let result = process_slash_command("/config", Some("cmd-1".to_string()), get_commands());

        assert!(!result.should_query);
        assert!(result.messages.is_empty());
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::OpenLocalCommandUi {
                command: LocalCommandUi::Settings {
                    default_tab: LocalSettingsTab::Config,
                },
                invocation: SlashCommandInvocation::new("config", ""),
            })
        );
    }

    #[test]
    fn process_slash_command_usage_opens_settings_usage_tab() {
        let result = process_slash_command("/usage", Some("cmd-1".to_string()), get_commands());

        assert!(!result.should_query);
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::OpenLocalCommandUi {
                command: LocalCommandUi::Settings {
                    default_tab: LocalSettingsTab::Usage,
                },
                invocation: SlashCommandInvocation::new("usage", ""),
            })
        );
    }

    #[test]
    fn process_slash_command_status_opens_settings_status_tab() {
        let result = process_slash_command("/status", Some("cmd-1".to_string()), get_commands());

        assert!(!result.should_query);
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::OpenLocalCommandUi {
                command: LocalCommandUi::Settings {
                    default_tab: LocalSettingsTab::Status,
                },
                invocation: SlashCommandInvocation::new("status", ""),
            })
        );
    }

    #[test]
    fn process_slash_command_recognizes_resume_arg_action() {
        let result = process_slash_command(
            "/continue abc123",
            Some("cmd-1".to_string()),
            get_commands(),
        );

        assert!(!result.should_query);
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::ResumeByArg {
                arg: "abc123".to_string(),
                invocation: SlashCommandInvocation::new("resume", "abc123"),
            })
        );
    }

    #[test]
    fn process_slash_command_rewind_opens_message_selector_like_official() {
        // Maps to: CC commands/rewind/rewind.ts — openMessageSelector + skip.
        let result = process_slash_command("/rewind", Some("cmd-r".to_string()), get_commands());

        assert!(!result.should_query);
        assert!(result.messages.is_empty(), "skip appends no messages");
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::OpenMessageSelector)
        );
    }

    #[test]
    fn process_slash_command_unknown_is_warning_and_non_querying() {
        let result = process_slash_command(
            "/does-not-exist abc",
            Some("u1".to_string()),
            get_commands(),
        );

        assert!(!result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(rows_after_caveat(&result)[0].uuid, "u1");
        assert!(warning_text(&result).contains("Unknown command: /does-not-exist"));
    }

    fn disabled() -> bool {
        false
    }

    #[test]
    fn process_slash_command_disabled_is_warning_and_non_querying() {
        let mut blocked = get_commands()[0].clone();
        blocked.name = std::borrow::Cow::Borrowed("blocked");
        blocked.aliases.clear();
        blocked.is_enabled = Some(disabled);
        blocked.call = None;

        let result = process_slash_command("/blocked", Some("u1".to_string()), &[blocked]);

        assert!(!result.should_query);
        assert!(result.local_action.is_none());
        assert!(warning_text(&result).contains("currently disabled"));
    }

    #[test]
    fn process_slash_command_clear_returns_ui_only_clear_action_with_transcript_rows() {
        let result = process_slash_command("/reset", Some("cmd-1".to_string()), get_commands());

        assert!(!result.should_query);
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::ClearConversation)
        );
        assert_eq!(rows_after_caveat(&result).len(), 2);
        // Wire pin: `<command-args>` is present even when empty.
        assert_eq!(
            user_text(&rows_after_caveat(&result)[0]),
            "<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>"
        );
        assert_eq!(
            local_output(&result),
            "<local-command-stdout></local-command-stdout>"
        );
    }

    #[cfg(feature = "anthropic_internal")]
    #[test]
    fn process_slash_command_version_outputs_product_version_for_internal_registry() {
        let registry = declared_commands_for_tests();
        let result = process_slash_command("/version", Some("cmd-1".to_string()), &registry);

        assert!(!result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(
            local_output(&result),
            format!(
                "<local-command-stdout>{}</local-command-stdout>",
                product::VERSION
            )
        );
    }

    #[test]
    fn process_slash_command_cost_reads_live_cost_tracker_state() {
        crate::cost_tracker::reset_cost_state_for_tests();
        crate::cost_tracker::add_to_total_cost(1.25, 4_200);
        crate::cost_tracker::add_to_total_tokens(7, 3);
        let result = process_slash_command("/cost", Some("cmd-1".to_string()), get_commands());

        assert!(!result.should_query);
        assert!(result.local_action.is_none());
        assert!(local_output(&result).contains("Total cost:            $1.25"));
        assert!(local_output(&result).contains("Total duration (API):  4s"));
        assert!(local_output(&result).contains("7 input, 3 output"));
        crate::cost_tracker::reset_cost_state_for_tests();
    }

    #[test]
    fn process_slash_command_compact_returns_typed_model_pipeline_action() {
        let result = process_slash_command(
            "/compact preserve imports",
            Some("cmd-1".to_string()),
            get_commands(),
        );

        assert!(!result.should_query);
        assert!(matches!(
            result.local_action,
            Some(SlashCommandAction::CompactConversation {
                custom_instructions,
                invocation: SlashCommandInvocation {
                    command_name,
                    args,
                },
                ..
            }) if custom_instructions == "preserve imports"
                && command_name == "compact"
                && args == "preserve imports"
        ));
    }

    #[test]
    fn process_slash_command_model_info_and_help_are_local_outputs() {
        let info =
            process_slash_command("/model current", Some("cmd-1".to_string()), get_commands());
        let help =
            process_slash_command("/model --help", Some("cmd-2".to_string()), get_commands());

        assert_eq!(
            local_output(&info),
            "<local-command-stdout>Current model: Sonnet 4.6 (default)</local-command-stdout>"
        );
        assert!(local_output(&help).contains("Run /model to open"));
        assert!(info.local_action.is_none());
        assert!(help.local_action.is_none());
    }

    #[test]
    fn process_slash_command_effort_help_matches_official_local_output() {
        let result = process_slash_command(
            "/effort --help",
            Some("cmd-effort".to_string()),
            get_commands(),
        );

        assert!(!result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(
            local_output(&result),
            format!(
                "<local-command-stdout>{}</local-command-stdout>",
                crate::commands::effort::effort::usage_text(
                    &crate::utils::model::model::get_main_loop_model()
                )
            )
        );
        assert_eq!(
            user_text(&rows_after_caveat(&result)[0]),
            "<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args>--help</command-args>"
        );
    }

    #[test]
    fn process_slash_command_context_defers_analysis_with_live_typed_context() {
        let typed = crate::types::message::Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "context history".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        });
        let context = crate::tool::ToolUseContext::default().with_messages(vec![typed.clone()]);
        let result = process_slash_command_with_context(
            "/context",
            Some("context-command".to_string()),
            get_commands(),
            &context,
            Vec::new(),
        );

        assert!(!result.should_query);
        assert!(result.messages.is_empty());
        assert!(matches!(
            result.local_action,
            Some(SlashCommandAction::AnalyzeContext { request, invocation })
                if request.context.messages == vec![typed]
                    && invocation == SlashCommandInvocation::new("context", "")
        ));
    }

    #[test]
    fn process_slash_command_plan_matches_enable_query_and_inspect_branches() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
        let enabled = process_slash_command_with_context(
            "/plan implement the migration",
            Some("plan-command".to_string()),
            get_commands(),
            &context,
            Vec::new(),
        );

        assert!(enabled.should_query);
        assert!(matches!(
            enabled.local_action,
            Some(SlashCommandAction::ApplyPlanMode { ref next_context })
                if next_context.mode == crate::types::permissions::PermissionMode::Plan
        ));
        // Both onDone branches send the formatCommandInputTags breadcrumb
        // (NOT a bare `/command`) followed by the wrapped stdout result
        // (CC processSlashCommand.tsx:779-806).
        assert_eq!(
            user_text(&enabled.messages[0]),
            "<command-name>/plan</command-name>\n            <command-message>plan</command-message>\n            <command-args>implement the migration</command-args>"
        );
        assert_eq!(
            user_text(&enabled.messages[1]),
            "<local-command-stdout>Enabled plan mode</local-command-stdout>"
        );
        assert_eq!(
            store.get().tool_permission_context.mode,
            crate::types::permissions::PermissionMode::Plan
        );

        let plan_context = crate::tool::ToolUseContext::default().with_app_store(store);
        let inspect = process_slash_command_with_context(
            "/plan",
            None,
            get_commands(),
            &plan_context,
            Vec::new(),
        );
        assert!(!inspect.should_query);
        assert!(inspect.messages.is_empty());
        assert!(matches!(
            inspect.local_action,
            Some(SlashCommandAction::InspectPlan { request, invocation })
                if request.args.is_empty()
                    && invocation == SlashCommandInvocation::new("plan", "")
        ));
    }

    #[test]
    fn process_slash_command_model_without_args_opens_ui_panel() {
        let result = process_slash_command("/model", Some("cmd-1".to_string()), get_commands());

        assert!(!result.should_query);
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::OpenLocalCommandUi {
                command: LocalCommandUi::Model,
                invocation: SlashCommandInvocation::new("model", ""),
            })
        );
    }

    #[test]
    fn process_slash_command_effort_without_args_opens_ui_panel() {
        let result =
            process_slash_command("/effort", Some("cmd-effort".to_string()), get_commands());

        assert!(!result.should_query);
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::OpenLocalCommandUi {
                command: LocalCommandUi::Effort {
                    args: None,
                    has_conversation_messages: false,
                },
                invocation: SlashCommandInvocation::new("effort", ""),
            })
        );
    }

    #[test]
    fn process_slash_command_local_command_ui_panels_are_wired() {
        let cases = [
            (
                "/allowed-tools",
                LocalCommandUi::Permissions,
                SlashCommandInvocation::new("permissions", ""),
            ),
            (
                "/memory",
                LocalCommandUi::Memory,
                SlashCommandInvocation::new("memory", ""),
            ),
            (
                "/agents",
                LocalCommandUi::Agents,
                SlashCommandInvocation::new("agents", ""),
            ),
            (
                "/skills",
                LocalCommandUi::Skills,
                SlashCommandInvocation::new("skills", ""),
            ),
            (
                "/doctor",
                LocalCommandUi::Doctor,
                SlashCommandInvocation::new("doctor", ""),
            ),
            (
                "/hooks",
                LocalCommandUi::Hooks,
                SlashCommandInvocation::new("hooks", ""),
            ),
            (
                "/diff",
                LocalCommandUi::Diff,
                SlashCommandInvocation::new("diff", ""),
            ),
            (
                "/login",
                LocalCommandUi::Login {
                    args: String::new(),
                },
                SlashCommandInvocation::new("login", ""),
            ),
            (
                "/logout",
                LocalCommandUi::Logout {
                    args: String::new(),
                },
                SlashCommandInvocation::new("logout", ""),
            ),
            (
                "/ide open",
                LocalCommandUi::Ide {
                    args: "open".to_string(),
                },
                SlashCommandInvocation::new("ide", "open"),
            ),
        ];

        for (input, expected, invocation) in cases {
            let result = process_slash_command(input, Some("cmd-1".to_string()), get_commands());
            assert!(!result.should_query, "{input}");
            assert_eq!(
                result.local_action,
                Some(SlashCommandAction::OpenLocalCommandUi {
                    command: expected,
                    invocation,
                }),
                "{input}"
            );
        }
    }

    #[test]
    fn process_slash_command_sandbox_matches_official_local_output_paths() {
        let panel =
            process_slash_command("/sandbox", Some("cmd-panel".to_string()), get_commands());
        assert!(!panel.should_query);
        assert_eq!(
            panel.local_action,
            Some(SlashCommandAction::OpenLocalCommandUi {
                command: LocalCommandUi::Sandbox,
                invocation: SlashCommandInvocation::new("sandbox", ""),
            })
        );

        let missing = process_slash_command(
            "/sandbox exclude",
            Some("cmd-sandbox".to_string()),
            get_commands(),
        );
        assert!(!missing.should_query);
        assert!(missing.local_action.is_none());
        assert!(local_output(&missing).contains("Please provide a command pattern"));
        // The error flag lives in the wire tag choice now.
        assert!(local_output(&missing).starts_with("<local-command-stderr>"));

        let unknown = process_slash_command(
            "/sandbox unknown",
            Some("cmd-sandbox-2".to_string()),
            get_commands(),
        );
        assert!(local_output(&unknown).contains("Unknown subcommand"));
    }

    #[test]
    fn process_slash_command_mcp_opens_official_local_ui_panel_for_all_args() {
        let base = process_slash_command("/mcp", Some("cmd-1".to_string()), get_commands());
        let enable = process_slash_command(
            "/mcp enable test-server",
            Some("cmd-2".to_string()),
            get_commands(),
        );
        let reconnect = process_slash_command(
            "/mcp reconnect docs",
            Some("cmd-3".to_string()),
            get_commands(),
        );

        for (result, args) in [
            (base, ""),
            (enable, "enable test-server"),
            (reconnect, "reconnect docs"),
        ] {
            assert!(!result.should_query, "{args}");
            assert!(result.messages.is_empty(), "{args}");
            assert_eq!(
                result.local_action,
                Some(SlashCommandAction::OpenLocalCommandUi {
                    command: LocalCommandUi::Mcp {
                        args: args.to_string(),
                    },
                    invocation: SlashCommandInvocation::new("mcp", args),
                }),
                "{args}"
            );
        }
    }

    #[test]
    fn process_slash_command_copy_receives_typed_history_like_official_context() {
        let context = crate::tool::ToolUseContext::default().with_messages(vec![
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "older response".to_string(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            }),
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "latest response".to_string(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ]);
        let result = process_slash_command_with_context(
            "/copy 2",
            Some("copy-command".to_string()),
            get_commands(),
            &context,
            Vec::new(),
        );

        assert!(result.messages.is_empty());
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::CopyToClipboard {
                content: crate::commands::copy::CopyContent {
                    text: "older response".to_string(),
                    filename: "response.md".to_string(),
                },
                invocation: SlashCommandInvocation::new("copy", "2"),
            })
        );
    }

    #[test]
    fn rename_receipts_match_official_system_display_without_caveat() {
        // CC rename/rename.ts:27-30,82 and processSlashCommand.tsx:783-798,
        // 675-681: normal onDone paths are two local_command System rows.
        let _env = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("rename-receipt-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _project = PinnedProjectDir::at(&root);
        struct RestoreSession(String, Option<std::path::PathBuf>, std::path::PathBuf);
        impl Drop for RestoreSession {
            fn drop(&mut self) {
                crate::utils::session_storage::clear_session_metadata();
                crate::utils::session_storage::reset_session_file_pointer();
                crate::bootstrap::state::switch_session(self.0.clone(), self.1.clone());
                let _ = std::fs::remove_dir_all(&self.2);
            }
        }
        let _session = RestoreSession(
            crate::bootstrap::state::get_session_id(),
            crate::bootstrap::state::get_session_project_dir(),
            root.clone(),
        );
        crate::bootstrap::state::switch_session(Uuid::new_v4().to_string(), Some(root));
        let _team_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        struct RestoreTeam(Option<crate::utils::teammate::DynamicTeamContext>);
        impl Drop for RestoreTeam {
            fn drop(&mut self) {
                crate::utils::teammate::set_dynamic_team_context(self.0.clone());
            }
        }
        let _restore = RestoreTeam(crate::utils::teammate::get_dynamic_team_context());
        for teammate in [false, true] {
            crate::utils::teammate::set_dynamic_team_context(teammate.then(|| {
                crate::utils::teammate::DynamicTeamContext {
                    agent_id: "agent-1".into(),
                    agent_name: "worker".into(),
                    team_name: "team".into(),
                    color: None,
                    plan_mode_required: false,
                    parent_session_id: None,
                }
            }));
            let store = crate::state::store::AppStore::new(Default::default(), None);
            let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
            let result = process_slash_command_with_context(
                "/rename  fixed-title  ",
                Some("rename-source-id".into()),
                get_commands(),
                &context,
                Vec::new(),
            );
            assert!(!result.should_query);
            assert!(result.local_action.is_none());
            assert_eq!(result.messages.len(), 2);
            assert_eq!(result.messages[0].uuid, "rename-source-id");
            assert!(
                result
                    .messages
                    .iter()
                    .all(crate::utils::messages::is_system_local_command_message)
            );
            let expected = if teammate {
                "Cannot rename: This session is a swarm teammate. Teammate names are set by the team leader."
            } else {
                "Session renamed to: fixed-title"
            };
            assert!(matches!(&result.messages[1].kind,
                RenderableMessageKind::System(SystemMessage::LocalCommand { content, .. })
                    if content == &format!("<local-command-stdout>{expected}</local-command-stdout>")));
            assert_eq!(
                store
                    .get()
                    .standalone_agent_context
                    .as_ref()
                    .map(|c| c.name.clone()),
                (!teammate).then(|| "fixed-title".to_string())
            );
        }
    }

    #[test]
    fn rename_failed_call_matches_official_empty_local_jsx_result() {
        // processSlashCommand.tsx:844-858 catches a failed call before onDone;
        // this native missing-AppStore boundary exercises that error carrier.
        let result = process_slash_command_with_context(
            "/rename unavailable",
            None,
            get_commands(),
            &crate::tool::ToolUseContext::default(),
            Vec::new(),
        );
        assert!(result.messages.is_empty());
        assert!(!result.should_query);
        assert!(result.local_action.is_none());
    }

    #[test]
    fn process_slash_command_rename_without_args_defers_official_name_generation() {
        let context = crate::tool::ToolUseContext::default().with_messages(vec![
            crate::types::message::Message::Assistant(crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "Fix the login flow".to_string(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            }),
        ]);
        let result = process_slash_command_with_context(
            "/rename",
            Some("rename-command".to_string()),
            get_commands(),
            &context,
            Vec::new(),
        );

        assert!(result.messages.is_empty());
        assert!(matches!(
            result.local_action,
            Some(SlashCommandAction::GenerateSessionName {
                request,
                invocation,
            }) if request.messages == context.messages
                && invocation == SlashCommandInvocation::new("rename", "")
        ));
    }

    #[test]
    fn process_slash_command_terminal_setup_matches_official_on_done_null_path() {
        let result =
            process_slash_command("/terminal-setup", Some("cmd-1".to_string()), get_commands());

        assert!(!result.should_query);
        // CC awaits setupTerminal before onDone produces transcript rows.
        assert!(result.messages.is_empty());
        assert_eq!(
            result.local_action,
            Some(SlashCommandAction::SetupTerminal {
                invocation: SlashCommandInvocation::new("terminal-setup", ""),
            })
        );
    }

    /// Maps to: CC `processSlashCommand.tsx:675-681`. `normalizeMessagesForAPI`
    /// turns `local_command` system rows into user messages so the model can
    /// reference earlier command output (`utils/messages.ts:2076-2092`), which
    /// is exactly why the caveat exists — without it that output reads as
    /// something the user said to Claude.
    #[test]
    fn local_command_results_carry_the_synthetic_caveat_but_system_display_ones_do_not() {
        let tag = crate::constants::xml::LOCAL_COMMAND_CAVEAT_TAG;
        let caveat_of = |result: &ProcessUserInputBaseResult| match &result.messages[0].kind {
            RenderableMessageKind::User { message } => match message.first_content_block() {
                // CC's `isMeta: true`.
                Some(crate::types::message::UserContent::MetaText(text))
                    if text.starts_with(&format!("<{tag}>")) =>
                {
                    Some(text.clone())
                }
                _ => None,
            },
            _ => None,
        };

        // Default branch (CC :792-806): user messages, so the caveat applies.
        let user_rows = process_slash_command("/reset", Some("c1".to_string()), get_commands());
        let caveat = caveat_of(&user_rows).expect("user-row command results get the caveat");
        assert!(caveat.contains("DO NOT respond to these messages"));

        // `display: 'system'` branch (CC :783-791): every row is
        // `local_command`, which CC exempts via `every(isSystemLocalCommandMessage)`.
        let system_rows =
            system_display_local_command_result(Some("c2".to_string()), "config", "", "output");
        assert!(
            system_rows
                .messages
                .iter()
                .all(crate::utils::messages::is_system_local_command_message),
            "precondition: this branch emits only local_command rows"
        );
        let mut exempted = system_rows;
        prepend_local_command_caveat(&mut exempted);
        assert!(caveat_of(&exempted).is_none());
    }

    #[test]
    fn system_display_matches_official_fullscreen_dismissal_filter() {
        // CC processSlashCommand.tsx:769-798: only system modal-dismissal
        // rows are omitted; default user output and other system output stay.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        for fullscreen in ["0", "1"] {
            let _env = EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", fullscreen);
            for output in [
                "Permissions dialog dismissed",
                "Workspace dialog dismissed",
                "changed permissions",
            ] {
                let result = system_display_local_command_result(None, "permissions", "", output);
                assert_eq!(
                    result.messages.len(),
                    if fullscreen == "1" && output.ends_with(" dismissed") {
                        0
                    } else {
                        2
                    }
                );
                assert!(!result.should_query);
                assert!(
                    result
                        .messages
                        .iter()
                        .all(crate::utils::messages::is_system_local_command_message)
                );
            }
            let mut result = model_visible_local_command_result(
                None,
                "permissions",
                "",
                "Added directory /workspace for this session",
                false,
                None,
            );
            prepend_local_command_caveat(&mut result);
            assert_eq!(result.messages.len(), 3);
            assert!(
                result
                    .messages
                    .iter()
                    .all(|row| matches!(row.kind, RenderableMessageKind::User { .. }))
            );
        }
    }
}

#[cfg(test)]
mod local_jsx_completion_tests {
    use super::*;

    #[test]
    fn reload_plugins_local_text_matches_official_user_system_and_error_rows() {
        let invocation = SlashCommandInvocation::new("reload-plugins", "ignored args");
        for (output, tag, expected) in [
            (
                Ok("Reloaded: 1 plugin".to_string()),
                "local-command-stdout",
                "Reloaded: 1 plugin",
            ),
            (
                Err("Error: load failed".to_string()),
                "local-command-stderr",
                "Error: load failed",
            ),
        ] {
            let mut prepared = local_text_command_input(&invocation);
            prepared
                .content
                .insert(0, UserContent::Text("preceding input".into()));
            let original = prepared.clone();
            let result = local_text_command_result(prepared, output);
            assert!(!result.should_query);
            assert!(result.local_action.is_none());
            assert_eq!(
                result.messages.len(),
                3,
                "one caveat, user input, system output"
            );
            let RenderableMessageKind::User { message } = &result.messages[1].kind else {
                panic!("local text input must retain a user envelope");
            };
            assert_eq!(
                message, &original,
                "await must preserve input UUID/timestamp"
            );
            assert_eq!(
                message.content,
                vec![
                    UserContent::Text("preceding input".into()),
                    UserContent::Text(format_command_input_tags("reload-plugins", "ignored args")),
                ]
            );
            assert!(matches!(&result.messages[2].kind,
                RenderableMessageKind::System(SystemMessage::LocalCommand { content, .. })
                if content == &format!("<{tag}>{expected}</{tag}>")));
        }
        let dispatch = process_slash_command_with_context(
            "/reload-plugins ignored args",
            None,
            &[crate::commands::reload_plugins::command()],
            &crate::tool::ToolUseContext::default(),
            vec![UserContent::Text("before".into())],
        );
        assert!(
            dispatch.messages.is_empty(),
            "deferred dispatch cannot mint a receipt early"
        );
        assert!(matches!(dispatch.local_action,
            Some(SlashCommandAction::ReloadPlugins { user_message, .. })
            if user_message.content.first() == Some(&UserContent::Text("before".into()))));
    }
    #[test]
    fn local_jsx_retry_completion_matches_official_optional_result_meta_and_skip() {
        use crate::utils::worktree::CommandResultDisplay;
        let result = local_jsx_command_result(
            "allowed-tools",
            "",
            None,
            None,
            true,
            &["Permission granted for: b, a".into()],
            Vec::new(),
        );
        assert!(result.should_query);
        assert_eq!(result.messages.len(), 3, "retry has no synthetic caveat");
        assert!(
            matches!(&result.messages[0].kind, RenderableMessageKind::User { message } if matches!(&message.content[0], UserContent::Text(text) if text.contains("/allowed-tools")))
        );
        assert!(
            matches!(&result.messages[1].kind, RenderableMessageKind::User { message } if message.content == vec![UserContent::Text("<local-command-stdout>(no content)</local-command-stdout>".into())])
        );
        assert!(
            matches!(&result.messages[2].kind, RenderableMessageKind::User { message } if message.content == vec![UserContent::MetaText("Permission granted for: b, a".into())])
        );
        let image = UserContent::Image {
            media_type: "image/png".into(),
            data: "controlled-base64".into(),
        };
        let with_image = local_jsx_command_result(
            "permissions",
            "",
            None,
            None,
            true,
            &[],
            vec![image.clone()],
        );
        let RenderableMessageKind::User { message } = &with_image.messages[0].kind else {
            panic!("command user message");
        };
        assert_eq!(message.content[0], image);
        assert!(
            matches!(&message.content[1], UserContent::Text(text) if text.contains("/permissions"))
        );
        assert!(
            message.image_paste_ids.is_none(),
            "source local-JSX createUserMessage supplies preceding blocks, not imagePasteIds"
        );
        let system = local_jsx_command_result(
            "permissions",
            "",
            Some("status"),
            Some(CommandResultDisplay::System),
            false,
            &[],
            vec![image],
        );
        assert!(
            system
                .messages
                .iter()
                .all(|row| matches!(row.kind, RenderableMessageKind::System(_)))
        );
        let skipped = local_jsx_command_result(
            "permissions",
            "",
            None,
            Some(CommandResultDisplay::Skip),
            true,
            &["hidden".into()],
            Vec::new(),
        );
        assert!(!skipped.should_query);
        assert!(skipped.messages.is_empty());
    }
    #[test]
    fn export_dispatch_matches_official_context_args_and_no_model_query() {
        let mut context = crate::tool::ToolUseContext::default();
        context.messages.push(crate::types::message::Message::User(
            crate::utils::messages::create_user_message("export source".into()),
        ));
        let command = crate::commands::declared_commands_for_tests()
            .into_iter()
            .find(|command| command.name == "export")
            .unwrap();
        let result = call_export(&command, "report.md", None, &context);
        assert!(!result.should_query);
        assert!(result.messages.is_empty());
        let Some(SlashCommandAction::ExportConversation {
            args,
            context: captured,
            invocation,
            ..
        }) = result.local_action
        else {
            panic!("missing deferred export")
        };
        assert_eq!(args, "report.md");
        assert_eq!(captured.messages, context.messages);
        assert_eq!(invocation.command_name, "export");
        assert_eq!(invocation.args, "report.md");
    }
}
