//! Attachment collection seam.
//! Maps to official `utils/attachments.ts` `getAttachmentMessages(...)` and
//! related post-tool attachment injection helpers used by `query.ts`.
//!
//! Claude Code injects attachment messages after a tool batch and before the
//! next model continuation. This owner now handles queued commands, critical
//! reminders, LSP diagnostics, deferred-tool/agent/MCP deltas, and the shared
//! `generateFileAttachment` policy used by post-compact restoration. Remaining
//! producers stay explicit here rather than growing ad-hoc query logic.

use crate::constants::query_source::QuerySource;
use crate::services::lsp::diagnostic_registry::{
    check_for_lsp_diagnostics, clear_all_lsp_diagnostics,
};
use crate::tool::ToolUseContext;
use crate::tools::bash_tool::tool_name::BASH_TOOL_NAME;
use crate::types::message::{AttachmentMessage, Message};
use crate::types::message::{RenderableMessage, RenderableMessageKind};
use crate::types::tools::tool_matches_name;
use serde::{Deserialize, Serialize};

// ─── The Attachment union ────────────────────────────────────────────────
//
// Maps to: CC `utils/attachments.ts:440-718` `export type Attachment` plus the
// named members declared beside it (`FileAttachment` :293, `CompactFileReferenceAttachment`
// :305, `PDFReferenceAttachment` :312, `AlreadyReadFileAttachment` :321,
// `AgentMentionAttachment` :399, `AsyncHookResponseAttachment` :404,
// `HookAttachment` :352-397 + :404-437, `TeammateMailboxAttachment` :719,
// `TeamContextAttachment` :730).
//
// `AttachmentMessage.attachment` is declared loosely in CC
// (`{type: string; [key: string]: unknown}`, types/message.ts:112) but every
// value CC ever stores is a member of this union. Rust keeps the loose
// declaration honest through [`Attachment::Unknown`]: any wire payload whose
// `type` is unrecognized — or whose shape does not parse (missing required
// fields, wrong field types) — is carried as the raw `serde_json::Value` and
// round-trips losslessly. One deliberate deviation from JS spread semantics:
// extra unknown fields on a payload that DOES parse into a typed member are
// dropped on re-serialization (serde ignores unknown fields); CC's runtime
// object spread would keep them. Recorded as a batch-D seam.
//
// Nested payload types CC imports from elsewhere are typed by reusing the
// Rust type that already models each one:
//   - `FileReadToolOutput` (FileReadTool.ts:333-335, `import { type Output
//     as FileReadToolOutput }` at attachments.ts:15) →
//     `crate::tools::file_read_tool::Output`
//   - `TodoList` (utils/todo/types.ts:17-18) → `Vec<TodoItem>`
//     (state/app_state_store.rs)
//   - `Task[]` (utils/tasks.ts:76-89) → `Vec<TaskRecord>` (utils/tasks.rs)
//   - `MemoryFileInfo` (utils/claudemd.ts:229-243) → `MemoryFileInfo`
//     (utils/claudemd.rs)
//   - `HookBlockingError` (utils/hooks.ts:330-333) → `HookBlockingError`
//     (services/hooks/mod.rs)
//   - `MessageOrigin` (types/message.ts:6) → `MessageOrigin` alias
//     (types/message.rs)
// Four payloads deliberately stay `serde_json::Value` — see the field docs
// on `queued_command.prompt` (`ContentBlockParam`), `skill_discovery.signal`
// (`DiscoverySignal`), `mcp_resource.content` (`ReadResourceResult`), and
// `async_hook_response.response` (`SyncHookJSONOutput`) for the per-field
// justification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Attachment {
    /// CC `FileAttachment` (utils/attachments.ts:293-303).
    #[serde(rename = "file", rename_all = "camelCase")]
    File {
        filename: String,
        /// CC `content: FileReadToolOutput` (FileReadTool.ts:333-335).
        content: crate::tools::file_read_tool::Output,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncated: Option<bool>,
        #[serde(default)]
        display_path: String,
    },
    /// CC `CompactFileReferenceAttachment` (utils/attachments.ts:305-310).
    #[serde(rename = "compact_file_reference", rename_all = "camelCase")]
    CompactFileReference {
        filename: String,
        #[serde(default)]
        display_path: String,
    },
    /// CC `PDFReferenceAttachment` (utils/attachments.ts:312-319).
    #[serde(rename = "pdf_reference", rename_all = "camelCase")]
    PdfReference {
        filename: String,
        page_count: u64,
        file_size: u64,
        #[serde(default)]
        display_path: String,
    },
    /// CC `AlreadyReadFileAttachment` (utils/attachments.ts:321-331).
    #[serde(rename = "already_read_file", rename_all = "camelCase")]
    AlreadyReadFile {
        filename: String,
        /// CC `content: FileReadToolOutput` (FileReadTool.ts:333-335).
        content: crate::tools::file_read_tool::Output,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncated: Option<bool>,
        #[serde(default)]
        display_path: String,
    },
    /// CC inline member (utils/attachments.ts:445-453).
    #[serde(rename = "edited_text_file", rename_all = "camelCase")]
    EditedTextFile { filename: String, snippet: String },
    /// CC inline member (utils/attachments.ts:454-458).
    #[serde(rename = "edited_image_file", rename_all = "camelCase")]
    EditedImageFile {
        filename: String,
        /// CC `content: FileReadToolOutput` (FileReadTool.ts:333-335).
        content: crate::tools::file_read_tool::Output,
    },
    /// CC inline member (utils/attachments.ts:459-465).
    #[serde(rename = "directory", rename_all = "camelCase")]
    Directory {
        path: String,
        content: String,
        #[serde(default)]
        display_path: String,
    },
    /// CC inline member (utils/attachments.ts:466-475).
    #[serde(rename = "selected_lines_in_ide", rename_all = "camelCase")]
    SelectedLinesInIde {
        ide_name: String,
        line_start: u64,
        line_end: u64,
        filename: String,
        content: String,
        #[serde(default)]
        display_path: String,
    },
    /// CC inline member (utils/attachments.ts:476-479).
    #[serde(rename = "opened_file_in_ide", rename_all = "camelCase")]
    OpenedFileInIde { filename: String },
    /// CC inline member (utils/attachments.ts:480-484); `content: TodoList`
    /// = `TodoItem[]` (utils/todo/types.ts:8-18).
    #[serde(rename = "todo_reminder", rename_all = "camelCase")]
    TodoReminder {
        content: Vec<crate::utils::todo::types::TodoItem>,
        item_count: usize,
    },
    /// CC inline member (utils/attachments.ts:485-489); `content: Task[]`
    /// (utils/tasks.ts:76-89 `TaskSchema`).
    #[serde(rename = "task_reminder", rename_all = "camelCase")]
    TaskReminder {
        content: Vec<crate::utils::tasks::TaskRecord>,
        item_count: usize,
    },
    /// CC inline member (utils/attachments.ts:490-495); `content:
    /// MemoryFileInfo` (utils/claudemd.ts:229-243).
    #[serde(rename = "nested_memory", rename_all = "camelCase")]
    NestedMemory {
        path: String,
        content: crate::utils::claudemd::MemoryFileInfo,
        #[serde(default)]
        display_path: String,
    },
    /// CC inline member (utils/attachments.ts:496-524).
    #[serde(rename = "relevant_memories", rename_all = "camelCase")]
    RelevantMemories { memories: Vec<RelevantMemory> },
    /// CC inline member (utils/attachments.ts:525-530).
    #[serde(rename = "dynamic_skill", rename_all = "camelCase")]
    DynamicSkill {
        skill_dir: String,
        skill_names: Vec<String>,
        #[serde(default)]
        display_path: String,
    },
    /// CC inline member (utils/attachments.ts:531-536).
    #[serde(rename = "skill_listing", rename_all = "camelCase")]
    SkillListing {
        content: String,
        skill_count: usize,
        is_initial: bool,
    },
    /// CC inline member (utils/attachments.ts:537-542).
    #[serde(rename = "skill_discovery", rename_all = "camelCase")]
    SkillDiscovery {
        skills: Vec<DiscoveredSkill>,
        /// CC `signal: DiscoverySignal` — kept as the wire value. The CC
        /// declaration is a generated stub inferred from usage
        /// (services/skillSearch/signals.ts:1-4), no Rust producer emits this
        /// attachment yet, and no consumer reads `signal` (render uses
        /// `skills` only, AttachmentMessage.tsx / attachment_message.rs).
        /// Typing an inferred stub shape would only risk degrading real
        /// payloads to `Unknown`.
        signal: serde_json::Value,
        source: String,
    },
    /// CC inline member (utils/attachments.ts:543-557).
    #[serde(rename = "queued_command", rename_all = "camelCase")]
    QueuedCommand {
        /// CC `prompt: string | Array<ContentBlockParam>` — kept as the wire
        /// value. The typed union exists
        /// (`anthropic_sdk::resources::messages::{ContentBlockParam,
        /// MessageContent}`) but derives no `PartialEq`/`Eq`, so it cannot
        /// enter this `Eq`-deriving union without an SDK-crate change
        /// (separate batch). Every consumer treats it generically anyway:
        /// `as_str()` + `value_to_text` (attachment_message.rs `prompt_text`,
        /// messages.rs::normalize_attachment_for_api, conversation_recovery.rs).
        prompt: serde_json::Value,
        #[serde(
            rename = "source_uuid",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        source_uuid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_paste_ids: Option<Vec<u64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_mode: Option<String>,
        /// CC `origin?: MessageOrigin` (types/message.ts:6 — an open string
        /// union; see the alias doc).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::types::message::MessageOrigin>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_meta: Option<bool>,
    },
    /// CC inline member (utils/attachments.ts:558-561).
    #[serde(rename = "output_style", rename_all = "camelCase")]
    OutputStyle { style: String },
    /// CC inline member (utils/attachments.ts:562-566).
    #[serde(rename = "diagnostics", rename_all = "camelCase")]
    Diagnostics {
        files: Vec<crate::services::lsp::types::DiagnosticFile>,
        is_new: bool,
    },
    /// CC inline member (utils/attachments.ts:567-573).
    #[serde(rename = "plan_mode", rename_all = "camelCase")]
    PlanMode {
        /// CC `'full' | 'sparse'`.
        reminder_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_sub_agent: Option<bool>,
        plan_file_path: String,
        plan_exists: bool,
    },
    /// CC inline member (utils/attachments.ts:574-577).
    #[serde(rename = "plan_mode_reentry", rename_all = "camelCase")]
    PlanModeReentry { plan_file_path: String },
    /// CC inline member (utils/attachments.ts:578-582).
    #[serde(rename = "plan_mode_exit", rename_all = "camelCase")]
    PlanModeExit {
        plan_file_path: String,
        plan_exists: bool,
    },
    /// CC inline member (utils/attachments.ts:583-586).
    #[serde(rename = "auto_mode", rename_all = "camelCase")]
    AutoMode { reminder_type: String },
    /// CC inline member (utils/attachments.ts:587-589).
    #[serde(rename = "auto_mode_exit")]
    AutoModeExit,
    /// CC inline member (utils/attachments.ts:590-593).
    #[serde(rename = "critical_system_reminder", rename_all = "camelCase")]
    CriticalSystemReminder { content: String },
    /// CC inline member (utils/attachments.ts:594-598).
    #[serde(rename = "plan_file_reference", rename_all = "camelCase")]
    PlanFileReference {
        plan_file_path: String,
        plan_content: String,
    },
    /// CC inline member (utils/attachments.ts:599-606).
    #[serde(rename = "mcp_resource", rename_all = "camelCase")]
    McpResource {
        server: String,
        uri: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// CC `content: ReadResourceResult` (imported from
        /// `@modelcontextprotocol/sdk/types.js`, utils/attachments.ts:81) —
        /// kept as the wire value. The MCP SDK schemas are `.passthrough()`
        /// (arbitrary extra keys are normal, not edge-case), the Rust client
        /// seam already yields a raw value
        /// (`services/mcp/client.rs#read_mcp_resource -> Value`), and the one
        /// consumer pokes `contents[].text/blob/mimeType` generically
        /// (messages.rs::normalize_attachment_for_api). A closed struct would
        /// systematically drop passthrough keys CC preserves.
        content: serde_json::Value,
    },
    /// CC inline member (utils/attachments.ts:607-611).
    #[serde(rename = "command_permissions", rename_all = "camelCase")]
    CommandPermissions {
        allowed_tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// CC `AgentMentionAttachment` (utils/attachments.ts:399-402).
    #[serde(rename = "agent_mention", rename_all = "camelCase")]
    AgentMention { agent_type: String },
    /// CC inline member (utils/attachments.ts:613-621).
    /// `delta_summary` is CC `string | null` (always present on the wire).
    #[serde(rename = "task_status", rename_all = "camelCase")]
    TaskStatus {
        task_id: String,
        task_type: String,
        status: String,
        description: String,
        #[serde(default)]
        delta_summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_file_path: Option<String>,
    },
    /// CC `AsyncHookResponseAttachment` (utils/attachments.ts:404-414).
    #[serde(rename = "async_hook_response", rename_all = "camelCase")]
    AsyncHookResponse {
        process_id: String,
        hook_name: String,
        hook_event: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        /// CC `response: SyncHookJSONOutput` — kept as the wire value. CC
        /// narrows via the `isSyncHookJSONOutput` type guard
        /// (types/hooks.ts:182-187), not a schema parse, so the ORIGINAL
        /// hook-stdout object (any extra keys included) is what CC stores and
        /// round-trips; the Rust producer mirrors that
        /// (`utils/hooks/async_hook_registry.rs` `AsyncHookResponse.
        /// response: Value`) and no consumer reads `response` structurally
        /// (render uses `hookEvent` only). A closed struct would drop keys
        /// CC keeps.
        response: serde_json::Value,
        #[serde(default)]
        stdout: String,
        #[serde(default)]
        stderr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
    },
    /// CC inline member (utils/attachments.ts:623-628).
    #[serde(rename = "token_usage", rename_all = "camelCase")]
    TokenUsage {
        used: u64,
        total: u64,
        remaining: u64,
    },
    /// CC inline member (utils/attachments.ts:629-634). Dollar amounts are
    /// floats; `serde_json::Number` keeps `Eq` derivable.
    #[serde(rename = "budget_usd", rename_all = "camelCase")]
    BudgetUsd {
        used: serde_json::Number,
        total: serde_json::Number,
        remaining: serde_json::Number,
    },
    /// CC inline member (utils/attachments.ts:635-640).
    /// `budget` is CC `number | null` (always present on the wire).
    #[serde(rename = "output_token_usage", rename_all = "camelCase")]
    OutputTokenUsage {
        turn: u64,
        session: u64,
        #[serde(default)]
        budget: Option<u64>,
    },
    /// CC inline member (utils/attachments.ts:641-644).
    #[serde(rename = "structured_output", rename_all = "camelCase")]
    StructuredOutput { data: serde_json::Value },
    /// CC `TeammateMailboxAttachment` (utils/attachments.ts:719-728).
    #[serde(rename = "teammate_mailbox", rename_all = "camelCase")]
    TeammateMailbox {
        messages: Vec<TeammateMailboxMessage>,
    },
    /// CC `TeamContextAttachment` (utils/attachments.ts:730-737).
    #[serde(rename = "team_context", rename_all = "camelCase")]
    TeamContext {
        agent_id: String,
        agent_name: String,
        team_name: String,
        team_config_path: String,
        task_list_path: String,
    },
    /// CC `HookCancelledAttachment` (utils/attachments.ts:432-437 + :393-397).
    #[serde(rename = "hook_cancelled", rename_all = "camelCase")]
    HookCancelled {
        hook_name: String,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// CC `HookAttachment` inline member (utils/attachments.ts:354-360);
    /// `blockingError: HookBlockingError` (utils/hooks.ts:330-333 — the
    /// `{blockingError, command}` object, not a bare string; see
    /// services/tools/toolHooks.ts:105-115).
    #[serde(rename = "hook_blocking_error", rename_all = "camelCase")]
    HookBlockingError {
        blocking_error: crate::services::hooks::HookBlockingError,
        hook_name: String,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
    },
    /// CC `HookNonBlockingErrorAttachment` (utils/attachments.ts:428-438).
    #[serde(rename = "hook_non_blocking_error", rename_all = "camelCase")]
    HookNonBlockingError {
        hook_name: String,
        stderr: String,
        stdout: String,
        exit_code: i64,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// CC `HookErrorDuringExecutionAttachment` (utils/attachments.ts:406-415).
    #[serde(rename = "hook_error_during_execution", rename_all = "camelCase")]
    HookErrorDuringExecution {
        content: String,
        hook_name: String,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// CC `HookAttachment` inline member (utils/attachments.ts:363-369).
    #[serde(rename = "hook_stopped_continuation", rename_all = "camelCase")]
    HookStoppedContinuation {
        message: String,
        hook_name: String,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
    },
    /// CC `HookSuccessAttachment` (utils/attachments.ts:416-427).
    #[serde(rename = "hook_success", rename_all = "camelCase")]
    HookSuccess {
        content: String,
        hook_name: String,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// CC `HookAttachment` inline member (utils/attachments.ts:371-377).
    #[serde(rename = "hook_additional_context", rename_all = "camelCase")]
    HookAdditionalContext {
        content: Vec<String>,
        hook_name: String,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
    },
    /// CC `HookSystemMessageAttachment` (utils/attachments.ts:385-391).
    #[serde(rename = "hook_system_message", rename_all = "camelCase")]
    HookSystemMessage {
        content: String,
        hook_name: String,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
    },
    /// CC `HookPermissionDecisionAttachment` (utils/attachments.ts:378-383).
    #[serde(rename = "hook_permission_decision", rename_all = "camelCase")]
    HookPermissionDecision {
        /// CC `'allow' | 'deny'`.
        decision: String,
        #[serde(rename = "toolUseID", default)]
        tool_use_id: String,
        hook_event: String,
    },
    /// CC inline member (utils/attachments.ts:645-652).
    #[serde(rename = "invoked_skills", rename_all = "camelCase")]
    InvokedSkills { skills: Vec<InvokedSkill> },
    /// CC inline member (utils/attachments.ts:653-655).
    #[serde(rename = "verify_plan_reminder")]
    VerifyPlanReminder,
    /// CC inline member (utils/attachments.ts:656-660).
    #[serde(rename = "max_turns_reached", rename_all = "camelCase")]
    MaxTurnsReached { max_turns: u32, turn_count: u32 },
    /// CC inline member (utils/attachments.ts:661-666).
    #[serde(rename = "current_session_memory", rename_all = "camelCase")]
    CurrentSessionMemory {
        content: String,
        path: String,
        token_count: u64,
    },
    /// CC inline member (utils/attachments.ts:667-670).
    #[serde(rename = "teammate_shutdown_batch", rename_all = "camelCase")]
    TeammateShutdownBatch { count: usize },
    /// CC inline member (utils/attachments.ts:671-673).
    #[serde(rename = "compaction_reminder")]
    CompactionReminder,
    /// CC inline member (utils/attachments.ts:674-676).
    #[serde(rename = "context_efficiency")]
    ContextEfficiency,
    /// CC inline member (utils/attachments.ts:677-680).
    #[serde(rename = "date_change", rename_all = "camelCase")]
    DateChange { new_date: String },
    /// CC inline member (utils/attachments.ts:681-684).
    #[serde(rename = "ultrathink_effort", rename_all = "camelCase")]
    UltrathinkEffort { level: String },
    /// CC inline member (utils/attachments.ts:685-690).
    #[serde(rename = "deferred_tools_delta", rename_all = "camelCase")]
    DeferredToolsDelta {
        added_names: Vec<String>,
        added_lines: Vec<String>,
        removed_names: Vec<String>,
    },
    /// CC inline member (utils/attachments.ts:691-701).
    #[serde(rename = "agent_listing_delta", rename_all = "camelCase")]
    AgentListingDelta {
        added_types: Vec<String>,
        added_lines: Vec<String>,
        removed_types: Vec<String>,
        is_initial: bool,
        show_concurrency_note: bool,
    },
    /// CC inline member (utils/attachments.ts:702-707).
    #[serde(rename = "mcp_instructions_delta", rename_all = "camelCase")]
    McpInstructionsDelta {
        added_names: Vec<String>,
        added_blocks: Vec<String>,
        removed_names: Vec<String>,
    },
    /// CC inline member (utils/attachments.ts:708-712).
    #[serde(rename = "companion_intro", rename_all = "camelCase")]
    CompanionIntro { name: String, species: String },
    /// CC inline member (utils/attachments.ts:713-718).
    #[serde(rename = "bagel_console", rename_all = "camelCase")]
    BagelConsole {
        error_count: u64,
        warning_count: u64,
        sample: String,
    },
    /// Rust-only lossless carrier for CC's loose declaration
    /// (`{type: string; [key: string]: unknown}`, types/message.ts:112).
    /// Holds any wire payload whose tag or shape is not (yet) typed above so
    /// nothing is dropped on round-trip. Must stay last: serde tries the
    /// tagged members first and falls back here.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

/// CC `relevant_memories.memories[number]` (utils/attachments.ts:498-524).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelevantMemory {
    pub path: String,
    #[serde(default)]
    pub content: String,
    /// CC `mtimeMs: number` — `Option` tolerates resumed pre-field sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<u64>,
    /// CC `header?: string` (pre-computed age header, cache-stability note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// CC `limit?: number` (truncation line count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

/// CC `skill_discovery.skills[number]` (utils/attachments.ts:538-539).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredSkill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_id: Option<String>,
}

/// CC `invoked_skills.skills[number]` (utils/attachments.ts:646-651).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokedSkill {
    pub name: String,
    pub path: String,
    pub content: String,
}

/// CC `teammate_mailbox.messages[number]` (utils/attachments.ts:720-727).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeammateMailboxMessage {
    pub from: String,
    pub text: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl Attachment {
    /// Parse a wire value into the typed union. Total: anything that does not
    /// match a typed member lands in [`Attachment::Unknown`] unchanged, which
    /// is CC's loose-declaration semantics (types/message.ts:112).
    pub fn from_wire(value: serde_json::Value) -> Self {
        match serde_json::from_value(value.clone()) {
            Ok(attachment) => attachment,
            Err(_) => Attachment::Unknown(value),
        }
    }

    /// The CC wire discriminant (`attachment.type`).
    pub fn wire_type(&self) -> &str {
        match self {
            Attachment::File { .. } => "file",
            Attachment::CompactFileReference { .. } => "compact_file_reference",
            Attachment::PdfReference { .. } => "pdf_reference",
            Attachment::AlreadyReadFile { .. } => "already_read_file",
            Attachment::EditedTextFile { .. } => "edited_text_file",
            Attachment::EditedImageFile { .. } => "edited_image_file",
            Attachment::Directory { .. } => "directory",
            Attachment::SelectedLinesInIde { .. } => "selected_lines_in_ide",
            Attachment::OpenedFileInIde { .. } => "opened_file_in_ide",
            Attachment::TodoReminder { .. } => "todo_reminder",
            Attachment::TaskReminder { .. } => "task_reminder",
            Attachment::NestedMemory { .. } => "nested_memory",
            Attachment::RelevantMemories { .. } => "relevant_memories",
            Attachment::DynamicSkill { .. } => "dynamic_skill",
            Attachment::SkillListing { .. } => "skill_listing",
            Attachment::SkillDiscovery { .. } => "skill_discovery",
            Attachment::QueuedCommand { .. } => "queued_command",
            Attachment::OutputStyle { .. } => "output_style",
            Attachment::Diagnostics { .. } => "diagnostics",
            Attachment::PlanMode { .. } => "plan_mode",
            Attachment::PlanModeReentry { .. } => "plan_mode_reentry",
            Attachment::PlanModeExit { .. } => "plan_mode_exit",
            Attachment::AutoMode { .. } => "auto_mode",
            Attachment::AutoModeExit => "auto_mode_exit",
            Attachment::CriticalSystemReminder { .. } => "critical_system_reminder",
            Attachment::PlanFileReference { .. } => "plan_file_reference",
            Attachment::McpResource { .. } => "mcp_resource",
            Attachment::CommandPermissions { .. } => "command_permissions",
            Attachment::AgentMention { .. } => "agent_mention",
            Attachment::TaskStatus { .. } => "task_status",
            Attachment::AsyncHookResponse { .. } => "async_hook_response",
            Attachment::TokenUsage { .. } => "token_usage",
            Attachment::BudgetUsd { .. } => "budget_usd",
            Attachment::OutputTokenUsage { .. } => "output_token_usage",
            Attachment::StructuredOutput { .. } => "structured_output",
            Attachment::TeammateMailbox { .. } => "teammate_mailbox",
            Attachment::TeamContext { .. } => "team_context",
            Attachment::HookCancelled { .. } => "hook_cancelled",
            Attachment::HookBlockingError { .. } => "hook_blocking_error",
            Attachment::HookNonBlockingError { .. } => "hook_non_blocking_error",
            Attachment::HookErrorDuringExecution { .. } => "hook_error_during_execution",
            Attachment::HookStoppedContinuation { .. } => "hook_stopped_continuation",
            Attachment::HookSuccess { .. } => "hook_success",
            Attachment::HookAdditionalContext { .. } => "hook_additional_context",
            Attachment::HookSystemMessage { .. } => "hook_system_message",
            Attachment::HookPermissionDecision { .. } => "hook_permission_decision",
            Attachment::InvokedSkills { .. } => "invoked_skills",
            Attachment::VerifyPlanReminder => "verify_plan_reminder",
            Attachment::MaxTurnsReached { .. } => "max_turns_reached",
            Attachment::CurrentSessionMemory { .. } => "current_session_memory",
            Attachment::TeammateShutdownBatch { .. } => "teammate_shutdown_batch",
            Attachment::CompactionReminder => "compaction_reminder",
            Attachment::ContextEfficiency => "context_efficiency",
            Attachment::DateChange { .. } => "date_change",
            Attachment::UltrathinkEffort { .. } => "ultrathink_effort",
            Attachment::DeferredToolsDelta { .. } => "deferred_tools_delta",
            Attachment::AgentListingDelta { .. } => "agent_listing_delta",
            Attachment::McpInstructionsDelta { .. } => "mcp_instructions_delta",
            Attachment::CompanionIntro { .. } => "companion_intro",
            Attachment::BagelConsole { .. } => "bagel_console",
            Attachment::Unknown(value) => value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
        }
    }

    /// The CC wire form (what `JSON.stringify(attachment)` would produce).
    pub fn to_wire(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

impl From<serde_json::Value> for Attachment {
    fn from(value: serde_json::Value) -> Self {
        Attachment::from_wire(value)
    }
}

/// A post-tool attachment yielded to the UI and appended to the model history.
/// Maps to CC `query.ts` `yield attachment; toolResults.push(attachment)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentContinuation {
    pub renderable_message: RenderableMessage,
    pub model_message: Message,
}

/// Parameters for the post-tool attachment pass.
/// Maps to CC `getAttachmentMessages(null, updatedToolUseContext, null,
/// queuedCommandsSnapshot, [...messagesForQuery, ...assistantMessages,
/// ...toolResults], querySource)`.
pub struct GetAttachmentMessagesParams<'a> {
    pub tool_use_context: &'a mut ToolUseContext,
    pub messages: &'a [Message],
    pub query_source: &'a QuerySource,
    /// Maps to: CC `getAttachments` 4th param `queuedCommands`
    /// (utils/attachments.ts:747). The drain lives with the caller: query.rs
    /// mid-turn passes its snapshot (CC query.ts:1566-1578); turn-0 callers
    /// pass an empty vec — "queuedCommands - handled by query.ts for mid-turn
    /// attachments" (processUserInput.ts:508, processSlashCommand.tsx:1230).
    pub queued_commands: Vec<crate::utils::message_queue_manager::QueuedCommand>,
}

/// Maps to CC `utils/attachments.ts#generateFileAttachment` `mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileAttachmentMode {
    Compact,
    AtMention,
}

/// Maps to CC `utils/attachments.ts#generateFileAttachment` `options`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GenerateFileAttachmentOptions {
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
}

/// Maps to: CC `utils/attachments.ts#generateFileAttachment`.
///
/// `limits` is projected into the invocation-local `ToolUseContext` before
/// calling the same inherent `FileReadTool::call` used by generic execution.
/// Telemetry event names are omitted; they do not affect attachment policy or
/// payloads.
pub(crate) async fn generate_file_attachment(
    filename: &str,
    tool_use_context: &mut ToolUseContext,
    mode: FileAttachmentMode,
    options: GenerateFileAttachmentOptions,
    limits: crate::tools::file_read_tool::limits::FileReadingLimits,
) -> Option<AttachmentMessage> {
    const MAX_LINES_TO_READ: usize = 2_000;

    let cwd = tool_use_context.effective_cwd();
    if is_file_read_denied(filename, &tool_use_context.tool_permission_context, &cwd) {
        return None;
    }
    let path = std::path::Path::new(filename);
    let display_path = attachment_display_path(path, &cwd);

    if mode == FileAttachmentMode::AtMention {
        // CC keeps the synchronous size gate separate from the subsequent
        // async stat calls: injected implementations may differ by endpoint.
        let within_limit = crate::utils::fs_operations::get_fs_implementation()
            .stat_sync(path)
            .is_ok_and(|metadata| metadata.len() as f64 <= limits.max_size_bytes);
        let extension = path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let is_pdf = crate::utils::pdf_utils::is_pdf_extension(&extension);
        if !within_limit
            && !is_pdf
            && crate::utils::fs_operations::get_fs_implementation()
                .stat(path)
                .await
                .is_ok()
        {
            return None;
        }
        if is_pdf {
            let pending_stats = crate::utils::fs_operations::get_fs_implementation().stat(path);
            let page_count_path = path.to_path_buf();
            let pending_pages = tokio::task::spawn_blocking(move || {
                crate::utils::pdf::get_pdf_page_count(&page_count_path)
            });
            // Promise.all rejects as soon as stat fails; the independently
            // started pdfinfo task continues without delaying normal reading.
            let settled = futures::try_join!(pending_stats, async {
                Ok::<_, std::io::Error>(pending_pages.await.ok().flatten())
            });
            if let Ok((metadata, page_count)) = settled {
                let page_count = page_count.unwrap_or_else(|| metadata.len().div_ceil(100 * 1024));
                if page_count > crate::constants::api_limits::PDF_AT_MENTION_INLINE_THRESHOLD {
                    return Some(AttachmentMessage::new(serde_json::json!({
                        "type": "pdf_reference",
                        "filename": filename,
                        "pageCount": page_count,
                        "fileSize": metadata.len(),
                        "displayPath": display_path,
                    })));
                }
            }
        }
        if let Some(existing) = tool_use_context.read_file_state.get(path) {
            if let Ok(metadata) = crate::utils::fs_operations::get_fs_implementation()
                .stat(path)
                .await
            {
                let current_mtime = metadata.mtime_ms.floor() as i64;
                if existing.timestamp_ms == Some(current_mtime) {
                    let existing_content = existing.content.as_deref().unwrap_or_default();
                    let total_lines = existing_content.matches('\n').count().saturating_add(1);
                    return Some(AttachmentMessage::new(serde_json::json!({
                        "type": "already_read_file",
                        "filename": filename,
                        "displayPath": display_path,
                        "content": {
                            "type": "text",
                            "file": {
                                "filePath": filename,
                                "content": existing_content,
                                "numLines": total_lines,
                                "startLine": options.offset.unwrap_or(1),
                                "totalLines": total_lines,
                            }
                        }
                    })));
                }
            }
        }
    }

    let mut args = serde_json::json!({"file_path": filename});
    if let Some(offset) = options.offset {
        args["offset"] = serde_json::json!(offset);
    }
    if let Some(limit) = options.limit {
        args["limit"] = serde_json::json!(limit);
    }
    let mut read_context = tool_use_context.clone();
    read_context.file_reading_limits = Some(crate::tool::FileReadingLimitsOverride {
        max_tokens: Some(limits.max_tokens),
        max_size_bytes: Some(limits.max_size_bytes),
    });
    let validate = |args: &serde_json::Value| {
        <crate::tools::file_read_tool::FileReadTool as crate::tool::ToolCall>::validate_input(
            &crate::tools::file_read_tool::FileReadTool,
            args,
            &read_context,
        )
    };
    if !matches!(validate(&args), crate::tool::ValidationResult::Ok) {
        return None;
    }

    let mut truncated = false;
    let output = match crate::tools::file_read_tool::FileReadTool
        .call(&args, &read_context, None)
        .await
    {
        Ok(output) => output,
        Err(error) if error.is_max_token_exceeded() || error.is_file_too_large() => {
            if mode == FileAttachmentMode::Compact {
                return Some(AttachmentMessage::new(serde_json::json!({
                    "type": "compact_file_reference",
                    "filename": filename,
                    "displayPath": display_path,
                })));
            }
            truncated = true;
            let truncated_args = serde_json::json!({
                "file_path": filename,
                "offset": options.offset.unwrap_or(1),
                "limit": MAX_LINES_TO_READ,
            });
            if !matches!(validate(&truncated_args), crate::tool::ValidationResult::Ok) {
                return None;
            }
            crate::tools::file_read_tool::FileReadTool
                .call(&truncated_args, &read_context, None)
                .await
                .ok()?
        }
        Err(_) => return None,
    };

    Some(AttachmentMessage::new(file_attachment_payload(
        filename,
        &display_path,
        output.data.to_output_schema_value(),
        truncated,
    )))
}

// ─── @file / @dir attachments ────────────────────────────────────────────
// Maps to CC `attachments.ts:1897-1963,2757-2860,2996-3199`.

static QUOTED_AT_MENTION: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r#"(?m)(^|\s)@\"([^\"]+)\""#).unwrap());
static REGULAR_AT_MENTION: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"(?m)(^|\s)@([^\s]+)\b").unwrap());
static AT_MENTION_LINES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^([^#]+)(?:#L(\d+)(?:-(\d+))?)?(?:#[^#]*)?$").unwrap()
});

/// Maps to: CC `utils/attachments.ts:2757-2791` `extractAtMentionedFiles`.
pub(crate) fn extract_at_mentioned_files(content: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for captures in QUOTED_AT_MENTION.captures_iter(content) {
        let Some(value) = captures.get(2).map(|capture| capture.as_str()) else {
            continue;
        };
        if !value.ends_with(" (agent)") && seen.insert(value.to_string()) {
            result.push(value.to_string());
        }
    }
    for captures in REGULAR_AT_MENTION.captures_iter(content) {
        let Some(value) = captures.get(2).map(|capture| capture.as_str()) else {
            continue;
        };
        if !value.starts_with('"') && seen.insert(value.to_string()) {
            result.push(value.to_string());
        }
    }
    result
}

/// Maps to: CC `utils/attachments.ts:2792-2801`
/// `extractMcpResourceMentions`.
pub(crate) fn extract_mcp_resource_mentions(content: &str) -> Vec<String> {
    static MCP_RESOURCE_MENTION: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?m)(?:^|\s)@([^\s]+:[^\s]+)\b").unwrap());
    let mut seen = std::collections::HashSet::new();
    MCP_RESOURCE_MENTION
        .captures_iter(content)
        .filter_map(|captures| captures.get(1).map(|capture| capture.as_str().to_string()))
        .filter(|mention| seen.insert(mention.clone()))
        .collect()
}

/// Maps to: CC `utils/attachments.ts:2802-2835` `extractAgentMentions`.
pub(crate) fn extract_agent_mentions(content: &str) -> Vec<String> {
    static QUOTED_AGENT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?m)(?:^|\s)@\"([\w:.@-]+) \(agent\)\""#).unwrap()
    });
    static UNQUOTED_AGENT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?m)(?:^|\s)@(agent-[\w:.@-]+)").unwrap());
    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();
    for captures in QUOTED_AGENT.captures_iter(content) {
        if let Some(agent) = captures.get(1).map(|capture| capture.as_str().to_string()) {
            if seen.insert(agent.clone()) {
                results.push(agent);
            }
        }
    }
    for captures in UNQUOTED_AGENT.captures_iter(content) {
        if let Some(agent) = captures.get(1).map(|capture| capture.as_str().to_string()) {
            if seen.insert(agent.clone()) {
                results.push(agent);
            }
        }
    }
    results
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AtMentionedFileLines {
    pub filename: String,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
}

/// Maps to: CC `utils/attachments.ts:2836-2853`
/// `parseAtMentionedFileLines`.
pub(crate) fn parse_at_mentioned_file_lines(mention: &str) -> AtMentionedFileLines {
    let Some(captures) = AT_MENTION_LINES.captures(mention) else {
        return AtMentionedFileLines {
            filename: mention.to_string(),
            line_start: None,
            line_end: None,
        };
    };
    let filename = captures
        .get(1)
        .map(|capture| capture.as_str())
        .unwrap_or(mention)
        .to_string();
    let parse_line =
        |capture: regex::Match<'_>| capture.as_str().parse::<usize>().unwrap_or(usize::MAX);
    let line_start = captures.get(2).map(parse_line);
    let line_end = captures.get(3).map(parse_line).or(line_start);
    AtMentionedFileLines {
        filename,
        line_start,
        line_end,
    }
}

fn attachment_display_path(path: &std::path::Path, cwd: &std::path::Path) -> String {
    path.strip_prefix(cwd)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn file_attachment_payload(
    filename: &str,
    display_path: &str,
    content: serde_json::Value,
    truncated: bool,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "type": "file",
        "filename": filename,
        "displayPath": display_path,
        "content": content,
    });
    if truncated {
        payload["truncated"] = serde_json::Value::Bool(true);
    }
    payload
}

/// Maps to: CC `utils/attachments.ts#isFileReadDenied`.
fn is_file_read_denied(
    file_path: &str,
    context: &crate::tool::ToolPermissionContext,
    cwd: &std::path::Path,
) -> bool {
    crate::utils::permissions::filesystem::matching_rule_for_input(
        file_path,
        context,
        crate::utils::permissions::filesystem::FilePermissionType::Read,
        crate::types::permissions::PermissionBehavior::Deny,
        cwd,
    )
    .is_some()
}

/// Maps to: CC `utils/attachments.ts#processAtMentionedFiles`.
async fn process_at_mentioned_files(
    input: &str,
    context: &mut ToolUseContext,
) -> Vec<AttachmentContinuation> {
    const MAX_DIR_ENTRIES: usize = 1_000;

    let cwd = context.effective_cwd();
    let mut attachments = Vec::new();
    for mention in extract_at_mentioned_files(input) {
        let parsed = parse_at_mentioned_file_lines(&mention);
        let path = crate::utils::path::expand_path(&parsed.filename, Some(&cwd))
            .unwrap_or_else(|_| cwd.join(&parsed.filename));
        let path_text = path.display().to_string();
        if is_file_read_denied(&path_text, &context.tool_permission_context, &cwd) {
            continue;
        }
        let display_path = attachment_display_path(&path, &cwd);
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
            let Ok(entries) = std::fs::read_dir(&path) else {
                continue;
            };
            let mut names = Vec::new();
            let mut entry_count = 0usize;
            for entry in entries.filter_map(Result::ok) {
                if context.abort_controller.is_aborted() {
                    break;
                }
                entry_count = entry_count.saturating_add(1);
                if names.len() < MAX_DIR_ENTRIES {
                    if let Ok(name) = entry.file_name().into_string() {
                        names.push(name);
                    }
                }
            }
            let truncated = entry_count.saturating_sub(MAX_DIR_ENTRIES);
            if truncated > 0 {
                names.push(format!("… and {truncated} more entries"));
            }
            let attachment = AttachmentMessage::new(serde_json::json!({
                "type": "directory",
                "path": path_text,
                "content": names.join("\n"),
                "displayPath": display_path,
            }));
            attachments.push(attachment_continuation(attachment));
            continue;
        }

        let offset = parsed.line_start;
        let limit = match (parsed.line_start, parsed.line_end) {
            (Some(start), Some(end)) if start != 0 && end != 0 && end >= start => {
                Some(end - start + 1)
            }
            (Some(start), Some(end)) if start != 0 && end != 0 && end < start => continue,
            _ => None,
        };
        let Some(attachment) = generate_file_attachment(
            &path_text,
            context,
            FileAttachmentMode::AtMention,
            GenerateFileAttachmentOptions { offset, limit },
            crate::tools::file_read_tool::limits::get_default_file_reading_limits(),
        )
        .await
        else {
            continue;
        };
        // The row is the typed model value; the renderer derives the
        // read-mode summary from the variant + `truncated` (batch D2).
        attachments.push(attachment_continuation(attachment));
    }
    attachments
}

#[derive(Debug)]
struct AtMentionWorkerResult {
    index: usize,
    attachments: Vec<AttachmentContinuation>,
}

/// Rust async-borrow adapter for CC `getAttachments`' concurrent file phase.
/// Worker clones retain the same Read-visible handles; no completion replay is
/// performed.
async fn process_at_mentioned_files_concurrently(
    input: &str,
    context: &mut ToolUseContext,
    deadline: tokio::time::Instant,
    attachment_abort: &crate::tool::AbortController,
) -> Vec<AttachmentContinuation> {
    let mentions = extract_at_mentioned_files(input);

    // CC starts every mention concurrently under the one shared getAttachments
    // abort signal. Completed siblings survive the timeout; only outstanding
    // work is discarded.
    let mut workers = tokio::task::JoinSet::new();
    for (index, mention) in mentions.into_iter().enumerate() {
        let mut worker_context = context.clone();
        worker_context.abort_controller = attachment_abort.clone();
        workers.spawn(async move {
            // Re-quote the already parsed mention so paths containing spaces
            // survive the worker's canonical mention parser.
            let attachments =
                process_at_mentioned_files(&format!("@\"{mention}\""), &mut worker_context).await;
            AtMentionWorkerResult { index, attachments }
        });
    }

    let mut completed = Vec::new();
    while !workers.is_empty() {
        match tokio::time::timeout_at(deadline, workers.join_next()).await {
            Ok(Some(Ok(result))) => completed.push(result),
            Ok(Some(Err(_))) => {}
            Ok(None) => break,
            Err(_) => {
                attachment_abort.abort();
                while let Some(result) = workers.join_next().await {
                    if let Ok(result) = result {
                        completed.push(result);
                    }
                }
                break;
            }
        }
    }

    // Promise.all preserves attachment result order. Read source-position
    // mutations already landed through the independently shared handles.
    let mut ordered_attachments = Vec::new();
    for result in completed {
        ordered_attachments.push((result.index, result.attachments));
    }
    ordered_attachments.sort_by_key(|(index, _)| *index);
    let attachments = ordered_attachments
        .into_iter()
        .flat_map(|(_, attachments)| attachments)
        .collect::<Vec<_>>();
    // The caller now runs the shared all-thread attachment pass after these
    // user-input attachments, matching CC's Promise.all phase boundary. That
    // pass consumes nested-memory/dynamic-skill triggers and emits the skill
    // listing in canonical order after changed_files.
    attachments
}

/// Maps to: CC `utils/attachments.ts:1995-2061`
/// `processMcpResourceAttachments`.
async fn process_mcp_resource_attachments(
    input: &str,
    context: &ToolUseContext,
) -> Vec<AttachmentContinuation> {
    let mentions = extract_mcp_resource_mentions(input);
    let workers = mentions.into_iter().map(|mention| async move {
        let (server_name, uri) = mention.split_once(':')?;
        if server_name.is_empty() || uri.is_empty() {
            return None;
        }
        let server = context.mcp_state.clients.iter().find(|server| {
            server.client.name == server_name
                && server.client.status
                    == crate::services::mcp::types::McpServerConnectionType::Connected
        })?;
        let resource = context
            .mcp_state
            .resources
            .get(server_name)
            .and_then(|resources| resources.iter().find(|resource| resource.uri == uri))
            .or_else(|| server.resources.iter().find(|resource| resource.uri == uri))?;
        let name = if resource.name.is_empty() {
            uri.to_string()
        } else {
            resource.name.clone()
        };
        let description = resource.description.clone();
        let content = crate::services::mcp::client::read_mcp_resource(server_name, uri)
            .await
            .ok()?;
        let attachment = AttachmentMessage::new(serde_json::json!({
            "type": "mcp_resource",
            "server": server_name,
            "uri": uri,
            "name": name,
            "description": description,
            "content": content,
        }));
        Some(attachment_continuation(attachment))
    });
    futures::future::join_all(workers)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Maps to: CC `utils/attachments.ts:1966-1993` `processAgentMentions`.
fn process_agent_mentions(input: &str, context: &ToolUseContext) -> Vec<AttachmentContinuation> {
    extract_agent_mentions(input)
        .into_iter()
        .filter_map(|mention| {
            let agent_type = mention.strip_prefix("agent-").unwrap_or(&mention);
            let definition = context
                .agent_definitions
                .active_agents
                .iter()
                .find(|definition| definition.agent_type == agent_type)?;
            Some(attachment_continuation(AttachmentMessage::new(
                serde_json::json!({
                    "type": "agent_mention",
                    "agentType": definition.agent_type,
                }),
            )))
        })
        .collect()
}

#[derive(Debug)]
struct UserInputAttachmentWorkerResult {
    index: usize,
    attachments: Vec<AttachmentContinuation>,
}

/// Maps to CC `getAttachments(...)`' user-input `Promise.all` phase. File,
/// MCP-resource, and agent mentions share one one-second boundary; completed
/// producers retain input order while Read effects mutate shared identities.
pub(crate) async fn process_user_input_attachments(
    input: &str,
    context: &mut ToolUseContext,
) -> Vec<AttachmentContinuation> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let attachment_abort = crate::tool::AbortController::default();
    let mut workers = tokio::task::JoinSet::new();
    let mut file_context = context.clone();
    file_context.abort_controller = attachment_abort.clone();
    let file_input = input.to_string();
    let file_abort = attachment_abort.clone();
    workers.spawn(async move {
        let attachments = process_at_mentioned_files_concurrently(
            &file_input,
            &mut file_context,
            deadline,
            &file_abort,
        )
        .await;
        UserInputAttachmentWorkerResult {
            index: 0,
            attachments,
        }
    });

    let mcp_input = input.to_string();
    let mut mcp_context = context.clone();
    mcp_context.abort_controller = attachment_abort.clone();
    workers.spawn(async move {
        UserInputAttachmentWorkerResult {
            index: 1,
            attachments: process_mcp_resource_attachments(&mcp_input, &mcp_context).await,
        }
    });

    let agent_input = input.to_string();
    let agent_context = context.clone();
    workers.spawn(async move {
        UserInputAttachmentWorkerResult {
            index: 2,
            attachments: process_agent_mentions(&agent_input, &agent_context),
        }
    });

    let mut completed = Vec::new();
    while !workers.is_empty() {
        match tokio::time::timeout_at(deadline, workers.join_next()).await {
            Ok(Some(Ok(result))) => completed.push(result),
            Ok(Some(Err(_))) => {}
            Ok(None) => break,
            Err(_) => {
                attachment_abort.abort();
                while let Some(result) = workers.join_next().await {
                    if let Ok(result) = result {
                        completed.push(result);
                    }
                }
                break;
            }
        }
    }

    let mut ordered = Vec::new();
    for result in completed {
        ordered.push((result.index, result.attachments));
    }
    ordered.sort_by_key(|(index, _)| *index);
    ordered
        .into_iter()
        .flat_map(|(_, attachments)| attachments)
        .collect()
}

fn announced_agent_types(messages: &[Message]) -> std::collections::HashSet<String> {
    let mut announced = std::collections::HashSet::new();
    for message in messages {
        let Message::Attachment(AttachmentMessage {
            attachment:
                Attachment::AgentListingDelta {
                    added_types,
                    removed_types,
                    ..
                },
            ..
        }) = message
        else {
            continue;
        };
        for name in added_types {
            announced.insert(name.clone());
        }
        for name in removed_types {
            announced.remove(name);
        }
    }
    announced
}

/// Maps to CC `utils/attachments.ts:1455-1481`
/// `getDeferredToolsDeltaAttachment(...)`.
/// Maps to: CC `utils/attachments.ts:1455-1489`
/// `getDeferredToolsDeltaAttachment`.
pub(crate) fn get_deferred_tools_delta_attachment(
    tools: &[crate::types::tools::Tool],
    model: &str,
    messages: &[Message],
) -> Option<AttachmentMessage> {
    let delta_enabled = crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Prompts,
    ) || crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::DeferredToolsDelta,
    );
    if !delta_enabled
        || !crate::tools::tool_search_tool::prompt::is_tool_search_enabled_optimistic()
        || !crate::tools::tool_search_tool::prompt::model_supports_tool_reference(model)
        || !crate::tools::tool_search_tool::prompt::is_tool_search_tool_available(tools)
    {
        return None;
    }

    let delta = crate::utils::tool_search::get_deferred_tools_delta(tools, messages)?;
    Some(AttachmentMessage::new(serde_json::json!({
        "type": "deferred_tools_delta",
        "addedNames": delta.added_names,
        "addedLines": delta.added_lines,
        "removedNames": delta.removed_names,
    })))
}

/// Maps to CC `utils/attachments.ts:1490-1556`
/// `getAgentListingDeltaAttachment(...)`.
/// Maps to: CC `utils/attachments.ts:1490-1558`
/// `getAgentListingDeltaAttachment`.
pub(crate) fn get_agent_listing_delta_attachment(
    context: &ToolUseContext,
    messages: &[Message],
) -> Option<AttachmentMessage> {
    if !crate::tools::agent_tool::prompt::should_inject_agent_list_in_messages()
        || !context
            .tools
            .iter()
            .any(|tool| tool_matches_name(tool, "Agent"))
    {
        return None;
    }

    let mcp_servers = context
        .tools
        .iter()
        .filter_map(|tool| {
            crate::services::mcp::mcp_string_utils::mcp_info_from_string(&tool.name)
                .map(|info| info.server_name)
        })
        .collect::<Vec<_>>();
    let filtered = crate::tools::agent_tool::load_agents_dir::filter_agents_by_mcp_requirements(
        &context.agent_definitions.active_agents,
        &mcp_servers,
    );
    let live_permission = context
        .get_app_state()
        .map(|state| (*state.tool_permission_context).clone())
        .unwrap_or_else(|| context.tool_permission_context.clone());
    let mut filtered = crate::utils::permissions::permissions::filter_denied_agents(
        &filtered,
        &live_permission,
        "Agent",
        |agent| agent.agent_type.as_str(),
    );
    // Maps to CC `attachments.ts:1503-1504` `const { activeAgents,
    // allowedAgentTypes } = toolUseContext.options.agentDefinitions` and
    // `:1519-1521` `if (allowedAgentTypes) filtered = filtered.filter(...)`:
    // main-thread agent frontmatter may expose Agent(type1,type2) while keeping
    // Agent itself in the tool pool. Unlike the AgentTool selection chain this
    // filter runs AFTER `filterDeniedAgents`.
    if let Some(allowed) = context.agent_definitions.allowed_agent_types.as_ref() {
        filtered.retain(|agent| allowed.contains(&agent.agent_type));
    }
    let announced = announced_agent_types(messages);
    let current_types = filtered
        .iter()
        .map(|agent| agent.agent_type.clone())
        .collect::<std::collections::HashSet<_>>();
    filtered.retain(|agent| !announced.contains(&agent.agent_type));
    filtered.sort_by(|left, right| left.agent_type.cmp(&right.agent_type));
    let mut removed_types = announced
        .iter()
        .filter(|name| !current_types.contains(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    removed_types.sort();
    if filtered.is_empty() && removed_types.is_empty() {
        return None;
    }

    let added_types = filtered
        .iter()
        .map(|agent| agent.agent_type.clone())
        .collect::<Vec<_>>();
    let added_lines = filtered
        .iter()
        .map(|agent| crate::tools::agent_tool::prompt::format_agent_line(agent))
        .collect::<Vec<_>>();
    let subscription = crate::utils::auth::get_subscription_type();
    Some(AttachmentMessage::new(serde_json::json!({
        "type": "agent_listing_delta",
        "addedTypes": added_types,
        "addedLines": added_lines,
        "removedTypes": removed_types,
        "isInitial": announced.is_empty(),
        "showConcurrencyNote": subscription.as_deref() != Some("pro"),
    })))
}

/// Maps to CC `utils/attachments.ts:1559-1587`
/// `getMcpInstructionsDeltaAttachment(...)` for server-authored instructions.
/// Maps to: CC `utils/attachments.ts:1559-1586`
/// `getMcpInstructionsDeltaAttachment`.
pub(crate) fn get_mcp_instructions_delta_attachment(
    context: &ToolUseContext,
    messages: &[Message],
) -> Option<AttachmentMessage> {
    if !crate::utils::mcp_instructions_delta::is_mcp_instructions_delta_enabled() {
        return None;
    }
    let connected_names = context
        .mcp_state
        .clients
        .iter()
        .filter(|server| {
            server.client.status == crate::services::mcp::types::McpServerConnectionType::Connected
        })
        .map(|server| server.client.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut instructions =
        crate::services::mcp::client::connected_mcp_server_instructions(&context.mcp_state);
    let model = context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let chrome_name = crate::utils::claude_in_chrome::common::CLAUDE_IN_CHROME_MCP_SERVER_NAME;
    if connected_names.contains(chrome_name)
        && crate::tools::tool_search_tool::prompt::is_tool_search_enabled_optimistic()
        && crate::tools::tool_search_tool::prompt::model_supports_tool_reference(&model)
        && crate::tools::tool_search_tool::prompt::is_tool_search_tool_available(&context.tools)
    {
        let chrome_prompt = crate::utils::claude_in_chrome::prompt::CHROME_TOOL_SEARCH_INSTRUCTIONS;
        instructions
            .entry(chrome_name.to_string())
            .and_modify(|server_prompt| {
                server_prompt.push_str("\n\n");
                server_prompt.push_str(chrome_prompt);
            })
            .or_insert_with(|| chrome_prompt.to_string());
    }
    let delta = crate::utils::mcp_instructions_delta::get_mcp_instructions_delta(
        &connected_names,
        &instructions,
        messages,
    )?;
    Some(AttachmentMessage::new(serde_json::json!({
        "type": "mcp_instructions_delta",
        "addedNames": delta.added_names,
        "addedBlocks": delta.added_blocks,
        "removedNames": delta.removed_names,
    })))
}

/// The typed model value IS the render row (batch D2): null-rendering types
/// are filtered by `nullRenderingAttachments`' predicate, exactly like CC.
fn attachment_continuation(attachment: AttachmentMessage) -> AttachmentContinuation {
    AttachmentContinuation {
        renderable_message: RenderableMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            kind: RenderableMessageKind::Attachment(attachment.attachment.clone()),
        },
        model_message: Message::Attachment(attachment),
    }
}

/// Maps to CC `getChangedFiles(...)` (`utils/attachments.ts:2063-2148`).
/// Maps to: CC `utils/attachments.ts:2063-2166` `getChangedFiles`.
pub(crate) async fn get_changed_files(context: &mut ToolUseContext) -> Vec<AttachmentContinuation> {
    let file_paths = context.read_file_state.keys();
    let cwd = context.effective_cwd();
    let context: &ToolUseContext = context;
    let pending = file_paths.into_iter().map(|file_path| {
        let cwd = &cwd;
        async move {
            // Maps to CC `cacheKeys(readFileState)` followed by the promoting
            // `readFileState.get(filePath)` inside each changed-file worker.
            let cache_path = std::path::PathBuf::from(&file_path);
            let Some(prior) = context.read_file_state.get(&cache_path) else {
                return None;
            };
            // Canonical currently watches only complete Edit/Write cache entries;
            // ranged/default-offset Read entries are skipped by its TODO guard.
            if prior.offset.is_some() || prior.limit.is_some() {
                return None;
            }
            let path = crate::utils::path::expand_path(&file_path, Some(cwd)).ok()?;
            let normalized_path = path.to_string_lossy().into_owned();
            if is_file_read_denied(&normalized_path, &context.tool_permission_context, cwd) {
                return None;
            }
            let metadata = match crate::utils::fs_operations::get_fs_implementation()
                .stat(&path)
                .await
            {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    context.read_file_state.delete(&cache_path);
                    return None;
                }
                Err(_) => return None,
            };
            let current_mtime = metadata.mtime_ms.floor() as i64;
            if prior
                .timestamp_ms
                .is_none_or(|timestamp| current_mtime <= timestamp)
            {
                return None;
            }
            let args = serde_json::json!({"file_path": normalized_path.clone()});
            if !matches!(
            <crate::tools::file_read_tool::FileReadTool as crate::tool::ToolCall>::validate_input(
                &crate::tools::file_read_tool::FileReadTool,
                &args,
                context,
            ),
            crate::tool::ValidationResult::Ok
        ) {
                return None;
            }
            let output = match crate::tools::file_read_tool::FileReadTool
                .call(&args, context, None)
                .await
            {
                Ok(output) => output,
                Err(error) if error.is_enoent() => {
                    context.read_file_state.delete(&cache_path);
                    return None;
                }
                Err(_) => return None,
            };
            match output.data {
                crate::tools::file_read_tool::ReadOutput::Text(output) => {
                    let Some(previous_content) = prior.content.as_deref() else {
                        return None;
                    };
                    let snippet =
                        crate::tools::file_edit_tool::utils::get_snippet_for_two_file_diff(
                            previous_content,
                            &output.content,
                        );
                    if snippet.is_empty() {
                        return None;
                    }
                    return Some(attachment_continuation(AttachmentMessage::new(
                        serde_json::json!({
                            "type": "edited_text_file",
                            "filename": normalized_path,
                            "snippet": snippet,
                        }),
                    )));
                }
                crate::tools::file_read_tool::ReadOutput::Image(_) => {
                    // CC deliberately performs a second helper-only image read here.
                    // It observes a replacement that lands after FileReadTool.call,
                    // but repeats none of Read's cache/skill/nested/listener effects.
                    let limits =
                        crate::tools::file_read_tool::limits::get_default_file_reading_limits();
                    let Ok(output) = crate::tools::file_read_tool::read_image_with_token_budget(
                        &path,
                        limits.max_tokens,
                        None,
                    ) else {
                        return None;
                    };
                    return Some(attachment_continuation(AttachmentMessage::new(
                        serde_json::json!({
                            "type": "edited_image_file",
                            "filename": normalized_path,
                            "content": {
                                "type": "image",
                                "file": {
                                    "base64": output.base64,
                                    "type": output.media_type,
                                    "originalSize": output.original_size,
                                    "dimensions": output.dimensions,
                                }
                            },
                        }),
                    )));
                }
                // notebook / pdf / parts have no canonical changed-file diff.
                _ => {}
            }
            None
        }
    });
    futures::future::join_all(pending)
        .await
        .into_iter()
        .flatten()
        .collect()
}

fn attachment_payload_type(message: &Message) -> Option<&str> {
    match message {
        Message::Attachment(attachment) => Some(attachment.attachment_type()),
        _ => None,
    }
}

fn is_human_turn(message: &Message) -> bool {
    matches!(
        message,
        Message::User(user)
            if user.content.iter().any(|content| matches!(content, crate::types::message::UserContent::Text(_)))
                && !user.content.iter().any(|content| matches!(content, crate::types::message::UserContent::ToolResult(_)))
    )
}

/// Maps to: CC `utils/attachments.ts:1186-1247` `getPlanModeAttachments`.
/// Its turn-count helpers remain inline with this source owner.
fn get_plan_mode_attachments(
    messages: &[Message],
    tool_use_context: &ToolUseContext,
) -> Vec<AttachmentContinuation> {
    if tool_use_context.tool_permission_context.mode
        != crate::types::permissions::PermissionMode::Plan
    {
        return Vec::new();
    }

    let mut turns_since_last_attachment = 0usize;
    let mut found_plan_attachment = false;
    for message in messages.iter().rev() {
        if is_human_turn(message) {
            turns_since_last_attachment = turns_since_last_attachment.saturating_add(1);
        } else if matches!(
            attachment_payload_type(message),
            Some("plan_mode" | "plan_mode_reentry")
        ) {
            found_plan_attachment = true;
            break;
        }
    }
    if found_plan_attachment && turns_since_last_attachment < 5 {
        return Vec::new();
    }

    let agent_id = tool_use_context.agent_id.as_deref();
    let plan_file_path = crate::utils::plans::get_plan_file_path(agent_id)
        .display()
        .to_string();
    let plan_exists = crate::utils::plans::get_plan(agent_id).is_some();
    let mut attachments = Vec::new();
    if crate::bootstrap::state::has_exited_plan_mode_in_session() && plan_exists {
        attachments.push(attachment_continuation(AttachmentMessage::new(
            serde_json::json!({
                "type": "plan_mode_reentry",
                "planFilePath": plan_file_path,
            }),
        )));
        crate::bootstrap::state::set_has_exited_plan_mode(false);
    }

    let prior_count = messages
        .iter()
        .rev()
        .take_while(|message| attachment_payload_type(message) != Some("plan_mode_exit"))
        .filter(|message| attachment_payload_type(message) == Some("plan_mode"))
        .count();
    let reminder_type = if (prior_count + 1) % 5 == 1 {
        "full"
    } else {
        "sparse"
    };
    attachments.push(attachment_continuation(AttachmentMessage::new(
        serde_json::json!({
            "type": "plan_mode",
            "reminderType": reminder_type,
            "isSubAgent": tool_use_context.agent_id.is_some(),
            "planFilePath": plan_file_path,
            "planExists": plan_exists,
        }),
    )));
    attachments
}

/// Maps to: CC `utils/attachments.ts:1248-1274`
/// `getPlanModeExitAttachment`.
fn get_plan_mode_exit_attachment(tool_use_context: &ToolUseContext) -> Vec<AttachmentContinuation> {
    if !crate::bootstrap::state::needs_plan_mode_exit_attachment() {
        return Vec::new();
    }
    crate::bootstrap::state::set_needs_plan_mode_exit_attachment(false);
    if tool_use_context.tool_permission_context.mode
        == crate::types::permissions::PermissionMode::Plan
    {
        return Vec::new();
    }
    let agent_id = tool_use_context.agent_id.as_deref();
    let plan_file_path = crate::utils::plans::get_plan_file_path(agent_id)
        .display()
        .to_string();
    let plan_exists = crate::utils::plans::get_plan(agent_id).is_some();
    vec![attachment_continuation(AttachmentMessage::new(
        serde_json::json!({
            "type": "plan_mode_exit",
            "planFilePath": plan_file_path,
            "planExists": plan_exists,
        }),
    ))]
}

/// Maps to: CC `utils/attachments.ts:743-994` `getAttachments`.
pub async fn get_attachments(
    params: GetAttachmentMessagesParams<'_>,
) -> Vec<AttachmentContinuation> {
    let GetAttachmentMessagesParams {
        tool_use_context,
        messages,
        query_source,
        queued_commands,
    } = params;

    // Maps to: CC `utils/attachments.ts:752-761` — bare/simple mode skips the
    // whole attachment pass but still projects the queued commands: the query
    // loop drained them unconditionally, so returning empty here would
    // silently drop them (Coworker runs with --bare and depends on
    // task-notification for mid-tool-call notifications).
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_ATTACHMENTS")
            .as_deref(),
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
    ) {
        return get_queued_command_attachments(&queued_commands);
    }

    struct WorkerResult {
        index: usize,
        attachments: Vec<AttachmentContinuation>,
        loaded_nested_paths: Vec<String>,
    }

    let mut workers = tokio::task::JoinSet::new();
    let changed_context = tool_use_context.clone();
    workers.spawn(async move {
        let mut context = changed_context;
        let attachments = get_changed_files(&mut context).await;
        WorkerResult {
            index: 0,
            attachments,
            loaded_nested_paths: Vec::new(),
        }
    });

    let nested_context = tool_use_context.clone();
    workers.spawn(async move {
        let mut context = nested_context;
        let initial_loaded = context.loaded_nested_memory_paths.clone();
        let attachments = get_nested_memory_attachments(&mut context).await;
        let loaded_nested_paths = context
            .loaded_nested_memory_paths
            .into_iter()
            .filter(|path| !initial_loaded.contains(path))
            .collect();
        WorkerResult {
            index: 1,
            attachments,
            loaded_nested_paths,
        }
    });

    let dynamic_context = tool_use_context.clone();
    workers.spawn(async move {
        let mut context = dynamic_context;
        let attachments = get_dynamic_skill_attachments(&mut context).await;
        WorkerResult {
            index: 2,
            attachments,
            loaded_nested_paths: Vec::new(),
        }
    });

    let ide_context = tool_use_context.clone();
    let ide_query_source = query_source.clone();
    workers.spawn(async move {
        let attachments = get_diagnostic_attachments(&ide_context, &ide_query_source).await;
        WorkerResult {
            index: 3,
            attachments,
            loaded_nested_paths: Vec::new(),
        }
    });

    let mut completed = Vec::new();
    while let Some(result) = workers.join_next().await {
        if let Ok(result) = result {
            tool_use_context
                .loaded_nested_memory_paths
                .extend(result.loaded_nested_paths);
            completed.push((result.index, result.attachments));
        }
    }
    completed.sort_by_key(|(index, _)| *index);
    let mut completed = completed.into_iter();
    let changed_files = completed.next().map(|(_, value)| value).unwrap_or_default();
    let nested_memory = completed.next().map(|(_, value)| value).unwrap_or_default();
    let dynamic_skills = completed.next().map(|(_, value)| value).unwrap_or_default();
    let ide_diagnostics = completed.next().map(|(_, value)| value).unwrap_or_default();

    let mut attachments = Vec::new();
    attachments.extend(get_queued_command_attachments(&queued_commands));
    let model = tool_use_context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    if let Some(attachment) =
        get_deferred_tools_delta_attachment(&tool_use_context.tools, &model, messages)
    {
        attachments.push(attachment_continuation(attachment));
    }
    if let Some(attachment) = get_agent_listing_delta_attachment(tool_use_context, messages) {
        attachments.push(attachment_continuation(attachment));
    }
    if let Some(attachment) = get_mcp_instructions_delta_attachment(tool_use_context, messages) {
        attachments.push(attachment_continuation(attachment));
    }
    attachments.extend(changed_files);
    attachments.extend(nested_memory);
    attachments.extend(dynamic_skills);
    attachments.extend(get_skill_listing_attachments(tool_use_context));
    attachments.extend(get_plan_mode_attachments(messages, tool_use_context));
    attachments.extend(get_plan_mode_exit_attachment(tool_use_context));
    // CC runs the agent pending-message drain for every thread alongside the
    // queued-command projection (`maybe('agent_pending_messages', …)`,
    // utils/attachments.ts:916-918, just before critical_system_reminder) —
    // the two are parallel chains, not an either/or on query source.
    attachments.extend(get_agent_pending_message_attachments(tool_use_context));
    attachments.extend(get_critical_system_reminder_attachments(tool_use_context));
    attachments.extend(ide_diagnostics);
    attachments.extend(get_lsp_diagnostic_attachments(
        tool_use_context,
        query_source,
    ));
    attachments
}

/// Maps to: CC `utils/attachments.ts:2854-2882`
/// `getDiagnosticAttachments`.
async fn get_diagnostic_attachments(
    tool_use_context: &ToolUseContext,
    query_source: &QuerySource,
) -> Vec<AttachmentContinuation> {
    if query_source.is_agent()
        || !tool_use_context
            .tools
            .iter()
            .any(|tool| tool_matches_name(tool, BASH_TOOL_NAME))
    {
        return Vec::new();
    }
    let files = crate::services::diagnostic_tracking::get_new_diagnostics().await;
    if files.is_empty() {
        return Vec::new();
    }
    let attachment = AttachmentMessage::new(crate::types::message::Attachment::Diagnostics {
        files,
        is_new: true,
    });
    vec![AttachmentContinuation {
        renderable_message: RenderableMessage {
            uuid: format!("ide-diagnostics-{}", uuid::Uuid::new_v4()),
            kind: RenderableMessageKind::Attachment(attachment.attachment.clone()),
        },
        model_message: Message::Attachment(attachment),
    }]
}

/// Maps to: CC `utils/attachments.ts:1044` `INLINE_NOTIFICATION_MODES` — the
/// queue modes converted to attachments mid-turn (everything else stays
/// queued for the post-turn queue processor). `pub(crate)` because query.rs's
/// merged drain predicate also consults it (CC keeps the mode filter on the
/// consume/remove side; the merged single-step dequeue needs it up front).
pub(crate) const INLINE_NOTIFICATION_MODES: [&str; 2] = ["prompt", "task-notification"];

/// Maps to: CC `utils/attachments.ts:1046-1083` `getQueuedCommandAttachments`
/// — a pure projection over the caller-drained snapshot (the drain itself is
/// CC query.ts:1566-1578 / query.rs `drain_queued_commands_snapshot`).
fn get_queued_command_attachments(
    queued_commands: &[crate::utils::message_queue_manager::QueuedCommand],
) -> Vec<AttachmentContinuation> {
    queued_commands
        .iter()
        .filter(|command| INLINE_NOTIFICATION_MODES.contains(&command.mode.as_str()))
        .map(queued_command_attachment)
        .collect()
}

/// Maps to: CC `utils/attachments.ts:1060-1081` — the per-command
/// `queued_command` attachment payload. `pastedContents` image blocks
/// (:1061-1071) have no Rust carrier yet (`QueuedCommand` lacks the field;
/// paste/bridge seam), so `prompt` stays the plain string value.
fn queued_command_attachment(
    command: &crate::utils::message_queue_manager::QueuedCommand,
) -> AttachmentContinuation {
    attachment_continuation(AttachmentMessage::new(Attachment::QueuedCommand {
        prompt: serde_json::Value::String(command.value.clone()),
        // CC `source_uuid: _.uuid` (:1075).
        source_uuid: command.uuid.clone(),
        // CC `imagePasteIds: getImagePasteIds(_.pastedContents)` (:1076) —
        // absent until the pastedContents carrier exists.
        image_paste_ids: None,
        // CC `commandMode: _.mode` (:1077).
        command_mode: Some(command.mode.clone()),
        // CC `origin: _.origin` (:1078) — the Rust QueuedCommand has no
        // origin field yet. Current human/task-notification queue producers
        // therefore use the source commandMode fallback at normalization.
        origin: None,
        // CC `isMeta: _.isMeta` (:1079): enqueue sites only ever set `true`,
        // so a `false` projects back to the absent field.
        is_meta: command.is_meta.then_some(true),
    }))
}

/// Maps to: CC `utils/attachments.ts:1085-1100`
/// `getAgentPendingMessageAttachments` — coordinator→subagent pending
/// messages drain into `queued_command` attachments on the agent's own loop;
/// non-agent threads return empty via the `!agentId` guard.
fn get_agent_pending_message_attachments(
    tool_use_context: &ToolUseContext,
) -> Vec<AttachmentContinuation> {
    let Some(agent_id) = tool_use_context.agent_id.as_deref() else {
        return Vec::new();
    };
    crate::tasks::local_agent_task::drain_pending_messages(agent_id)
        .into_iter()
        .map(|message| {
            attachment_continuation(AttachmentMessage::new(Attachment::QueuedCommand {
                prompt: serde_json::Value::String(message),
                source_uuid: None,
                image_paste_ids: None,
                command_mode: None,
                // CC attachments.ts:1098 preserves the coordinator provenance.
                origin: Some(serde_json::json!({"kind":"coordinator"})),
                is_meta: Some(true),
            }))
        })
        .collect()
}

/// Maps to: CC `utils/attachments.ts#getCriticalSystemReminderAttachment`.
fn get_critical_system_reminder_attachments(
    tool_use_context: &ToolUseContext,
) -> Vec<AttachmentContinuation> {
    let Some(reminder) = tool_use_context
        .critical_system_reminder_experimental
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Vec::new();
    };
    let content = reminder.trim().to_string();
    let attachment = AttachmentMessage::new(Attachment::CriticalSystemReminder { content });
    vec![AttachmentContinuation {
        renderable_message: RenderableMessage {
            uuid: format!("critical-system-reminder-{}", uuid::Uuid::new_v4()),
            kind: RenderableMessageKind::Attachment(attachment.attachment.clone()),
        },
        model_message: Message::Attachment(attachment),
    }]
}

fn relative_display_path(from: &std::path::Path, to: &std::path::Path) -> String {
    use std::path::Component;

    let from_components = from.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();

    // Different Windows volumes/prefixes cannot be represented relatively;
    // Node returns the absolute destination in that case.
    if from_components
        .first()
        .zip(to_components.first())
        .is_some_and(|(left, right)| {
            matches!(left, Component::Prefix(_) | Component::RootDir)
                && matches!(right, Component::Prefix(_) | Component::RootDir)
                && left != right
        })
    {
        return to.display().to_string();
    }

    let mut relative = std::path::PathBuf::new();
    for component in &from_components[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &to_components[common..] {
        relative.push(component.as_os_str());
    }
    relative.display().to_string()
}

/// Maps to CC `utils/attachments.ts:getDirectoriesToProcess` (:1638-1701).
/// Maps to: CC `utils/attachments.ts:1656-1697` `getDirectoriesToProcess`.
fn get_directories_to_process(
    target_path: &std::path::Path,
    original_cwd: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let target_dir = target_path.parent().unwrap_or(target_path);
    let mut nested_dirs = Vec::new();
    let mut current = target_dir.to_path_buf();
    while current != original_cwd {
        let Some(parent) = current.parent() else {
            break;
        };
        if current.starts_with(original_cwd) {
            nested_dirs.push(current.clone());
        }
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    nested_dirs.reverse();
    nested_dirs
}

fn get_cwd_level_directories(original_cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut directories = original_cwd
        .ancestors()
        .take_while(|directory| directory.parent().is_some())
        .map(std::path::Path::to_path_buf)
        .collect::<Vec<_>>();
    directories.reverse();
    directories
}

fn claude_md_kind_name(kind: crate::utils::claudemd::ClaudeMdKind) -> &'static str {
    match kind {
        crate::utils::claudemd::ClaudeMdKind::Managed => "Managed",
        crate::utils::claudemd::ClaudeMdKind::User => "User",
        crate::utils::claudemd::ClaudeMdKind::Project => "Project",
        crate::utils::claudemd::ClaudeMdKind::Local => "Local",
        crate::utils::claudemd::ClaudeMdKind::AutoMem => "AutoMem",
        crate::utils::claudemd::ClaudeMdKind::TeamMem => "TeamMem",
    }
}

/// Maps to CC `memoryFilesToAttachments(...)` (:1724-1788).
/// Maps to: CC `utils/attachments.ts:1710-1791` `memoryFilesToAttachments`.
fn memory_files_to_attachments(
    memory_files: Vec<crate::utils::claudemd::ClaudeMdFile>,
    tool_use_context: &mut ToolUseContext,
    trigger_file_path: Option<&std::path::Path>,
) -> Vec<AttachmentContinuation> {
    let cwd = tool_use_context.effective_cwd();
    let mut attachments = Vec::new();
    for memory_file in memory_files {
        let cache_path = memory_file.path.clone();
        let path = cache_path.display().to_string();
        if tool_use_context.loaded_nested_memory_paths.contains(&path) {
            continue;
        }
        if tool_use_context.read_file_state.has(&memory_file.path) {
            continue;
        }
        let display_path = relative_display_path(&cwd, &memory_file.path);
        let content = memory_file.content.clone();
        let disk_projection = crate::utils::claudemd::claude_md_disk_projection(&memory_file);
        let instructions_hook = match memory_file.kind {
            crate::utils::claudemd::ClaudeMdKind::Managed => Some("Managed"),
            crate::utils::claudemd::ClaudeMdKind::User => Some("User"),
            crate::utils::claudemd::ClaudeMdKind::Project => Some("Project"),
            crate::utils::claudemd::ClaudeMdKind::Local => Some("Local"),
            crate::utils::claudemd::ClaudeMdKind::AutoMem
            | crate::utils::claudemd::ClaudeMdKind::TeamMem => None,
        }
        .map(|memory_type| {
            let load_reason = if disk_projection.globs.is_some() {
                "path_glob_match"
            } else if memory_file.parent.is_some() {
                "include"
            } else {
                "nested_traversal"
            };
            crate::services::hooks::instructions_loaded::InstructionsLoadedInput {
                file_path: path.clone(),
                memory_type,
                load_reason,
                globs: disk_projection.globs.clone(),
                trigger_file_path: trigger_file_path.map(|path| path.display().to_string()),
                parent_file_path: memory_file
                    .parent
                    .as_ref()
                    .map(|path| path.display().to_string()),
            }
        });
        // CC embeds the `MemoryFileInfo` object as `content`
        // (utils/attachments.ts:490-495,1710-1791); optional keys are present
        // only when set, matching JSON.stringify's undefined-omission.
        let memory_info = crate::utils::claudemd::MemoryFileInfo {
            path: path.clone(),
            memory_type: claude_md_kind_name(memory_file.kind).to_string(),
            content,
            parent: memory_file
                .parent
                .as_ref()
                .map(|parent| parent.display().to_string()),
            globs: disk_projection.globs.clone(),
            content_differs_from_disk: disk_projection.content_differs_from_disk.then_some(true),
            raw_content: disk_projection
                .content_differs_from_disk
                .then(|| disk_projection.raw_content.clone()),
        };
        let attachment = AttachmentMessage::new(Attachment::NestedMemory {
            path: path.clone(),
            content: memory_info,
            display_path,
        });
        tool_use_context
            .loaded_nested_memory_paths
            .insert(path.clone());
        tool_use_context.read_file_state.set(
            &cache_path,
            crate::utils::query_helpers::ReadFileStateEntry {
                path,
                content: Some(if disk_projection.content_differs_from_disk {
                    disk_projection.raw_content
                } else {
                    memory_file.content
                }),
                timestamp_ms: Some(chrono::Utc::now().timestamp_millis()),
                offset: None,
                limit: None,
                is_partial_view: disk_projection.content_differs_from_disk,
                source: crate::utils::query_helpers::ReadFileStateSource::NestedMemory,
            },
        );
        attachments.push(attachment_continuation(attachment));
        if let Some(input) = instructions_hook {
            crate::services::hooks::instructions_loaded::dispatch_instructions_loaded_hooks(input);
        }
    }
    attachments
}

/// Maps to: CC `utils/attachments.ts:2167-2195`
/// `getNestedMemoryAttachments`; delegates per-file work to the adjacent
/// source-named helper.
async fn get_nested_memory_attachments(
    tool_use_context: &mut ToolUseContext,
) -> Vec<AttachmentContinuation> {
    let Some(triggers) = tool_use_context.nested_memory_attachment_triggers.clone() else {
        return Vec::new();
    };
    if triggers.is_empty() {
        return Vec::new();
    }
    let original_cwd = crate::bootstrap::state::get_original_cwd();
    let effective_cwd = tool_use_context.effective_cwd();
    let mut permission_context = tool_use_context.tool_permission_context.clone();
    let effective_cwd_text = effective_cwd.display().to_string();
    permission_context
        .additional_working_directories
        .entry(effective_cwd_text.clone())
        .or_insert_with(|| crate::types::permissions::AdditionalWorkingDirectory {
            path: effective_cwd_text,
            source: crate::types::permissions::PermissionRuleSource::Session,
        });
    let mut attachments = Vec::new();
    let mut trigger_index = 0;
    // Each iterator step is its own source turn. Filesystem/memory processing
    // deliberately runs with the scheduling gate released so a concurrent
    // Read insertion before the terminal step can join this same live pass.
    while let Some(trigger) = triggers.next_or_clear(trigger_index) {
        trigger_index += 1;
        if !crate::utils::permissions::filesystem::path_in_allowed_working_path(
            &trigger,
            &permission_context,
            None,
        ) {
            continue;
        }
        let target =
            crate::utils::path::expand_path(&trigger, Some(&tool_use_context.effective_cwd()))
                .unwrap_or_else(|_| std::path::PathBuf::from(&trigger));
        let mut processed_paths = std::collections::HashSet::new();
        // Rust's memory discovery helpers are synchronous, so yield explicitly
        // at CC's first awaited discovery boundary. This lets a changed-file
        // Read insertion join the live Set before the terminal iterator step.
        tokio::task::yield_now().await;
        let managed_user_rules = crate::utils::claudemd::get_managed_and_user_conditional_rules(
            &target,
            &original_cwd,
            &mut processed_paths,
        );
        attachments.extend(memory_files_to_attachments(
            managed_user_rules,
            tool_use_context,
            Some(&target),
        ));
        for directory in get_directories_to_process(&target, &original_cwd) {
            tokio::task::yield_now().await;
            let files = crate::utils::claudemd::get_memory_files_for_nested_directory(
                &directory,
                &target,
                &original_cwd,
                &mut processed_paths,
            );
            attachments.extend(memory_files_to_attachments(
                files,
                tool_use_context,
                Some(&target),
            ));
        }
        for directory in get_cwd_level_directories(&original_cwd) {
            tokio::task::yield_now().await;
            let files = crate::utils::claudemd::get_conditional_rules_for_cwd_level_directory(
                &directory,
                &target,
                &original_cwd,
                &mut processed_paths,
            );
            attachments.extend(memory_files_to_attachments(
                files,
                tool_use_context,
                Some(&target),
            ));
        }
    }
    attachments
}

/// Maps to: CC `utils/attachments.ts` `getDynamicSkillAttachments(...)`.
async fn get_dynamic_skill_attachments(
    tool_use_context: &mut ToolUseContext,
) -> Vec<AttachmentContinuation> {
    let Some(triggers) = tool_use_context.dynamic_skill_dir_triggers.clone() else {
        return Vec::new();
    };
    // CC snapshots before I/O and clears the live Set only after that work.
    // Keep those as distinct source turns: an insertion after this snapshot is
    // not processed now, but is still removed by the later unconditional clear.
    let directories = triggers.snapshot();
    // Rust's directory helpers are synchronous; yield at CC's awaited readdir
    // boundary so live insertions can occur after snapshot and before clear.
    tokio::task::yield_now().await;
    let attachments = directories
        .into_iter()
        .filter_map(|skill_dir| {
            let skill_names = std::fs::read_dir(&skill_dir)
                .ok()?
                .flatten()
                .filter_map(|entry| {
                    let file_type = entry.file_type().ok()?;
                    if !(file_type.is_dir() || file_type.is_symlink()) {
                        return None;
                    }
                    std::fs::metadata(entry.path().join("SKILL.md"))
                        .ok()
                        .map(|_| entry.file_name().to_string_lossy().to_string())
                })
                .collect::<Vec<_>>();
            if skill_names.is_empty() {
                return None;
            }
            let display_path = relative_display_path(
                &tool_use_context.effective_cwd(),
                std::path::Path::new(&skill_dir),
            );
            let id = format!("dynamic-skill-{}", uuid::Uuid::new_v4());
            // CC normalizes dynamic_skill attachments to no API message;
            // retaining the local Attachment record preserves transcript/UI.
            let attachment = AttachmentMessage::new(Attachment::DynamicSkill {
                skill_dir,
                skill_names,
                display_path,
            });
            Some(AttachmentContinuation {
                renderable_message: RenderableMessage {
                    uuid: id,
                    kind: RenderableMessageKind::Attachment(attachment.attachment.clone()),
                },
                model_message: Message::Attachment(attachment),
            })
        })
        .collect();
    triggers.clear();
    attachments
}

static SENT_SKILL_NAMES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

const FILTERED_SKILL_LISTING_MAX: usize = 30;

/// Maps to CC `filterToBundledAndMcp(...)`.
/// Maps to: CC `utils/attachments.ts:2651-2660` `filterToBundledAndMcp`.
fn filter_to_bundled_and_mcp(
    commands: Vec<crate::commands::Command>,
) -> Vec<crate::commands::Command> {
    use crate::skills::load_skills_dir::SkillLoadedFrom;

    let filtered = commands
        .into_iter()
        .filter(|command| {
            matches!(
                command.loaded_from,
                Some(SkillLoadedFrom::Bundled | SkillLoadedFrom::Mcp)
            )
        })
        .collect::<Vec<_>>();
    if filtered.len() > FILTERED_SKILL_LISTING_MAX {
        filtered
            .into_iter()
            .filter(|command| command.loaded_from == Some(SkillLoadedFrom::Bundled))
            .collect()
    } else {
        filtered
    }
}

/// Maps to: CC `utils/attachments.ts:2661-2756`
/// `getSkillListingAttachments`.
fn get_skill_listing_attachments(
    tool_use_context: &mut ToolUseContext,
) -> Vec<AttachmentContinuation> {
    if crate::utils::process_env::var("NODE_ENV").as_deref() == Some("test") {
        return Vec::new();
    }
    if !tool_use_context
        .tools
        .iter()
        .any(|tool| tool_matches_name(tool, crate::tools::skill_tool::constants::SKILL_TOOL_NAME))
    {
        return Vec::new();
    }
    let cwd = crate::bootstrap::state::get_original_cwd();
    let mut commands = crate::commands::get_skill_tool_commands(&cwd);
    // CC reads `toolUseContext.getAppState().mcp.commands` here. The cloned
    // context snapshot remains the headless/test fallback when no live store
    // is attached, but an attached AppState must win so list_changed updates
    // are visible to the next attachment.
    let mcp_commands = tool_use_context
        .get_app_state()
        .map(|state| state.mcp.commands.clone())
        .unwrap_or_else(|| tool_use_context.mcp_state.commands.clone());
    for command in crate::commands::get_mcp_skill_commands(&mcp_commands) {
        if !commands
            .iter()
            .any(|existing| existing.name == command.name)
        {
            commands.push(command);
        }
    }
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::ExperimentalSkillSearch,
    ) {
        commands = filter_to_bundled_and_mcp(commands);
    }

    let agent_key = tool_use_context.agent_id.clone().unwrap_or_default();
    let mut sent_by_agent = SENT_SKILL_NAMES
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let sent = sent_by_agent.entry(agent_key).or_default();
    if tool_use_context
        .resume_restore_stores
        .skill_restore
        .suppress_next_skill_listing
    {
        tool_use_context
            .resume_restore_stores
            .skill_restore
            .suppress_next_skill_listing = false;
        sent.extend(commands.iter().map(|command| command.name.to_string()));
        return Vec::new();
    }
    let new_commands = commands
        .into_iter()
        .filter(|command| !sent.contains(command.name.as_ref()))
        .collect::<Vec<_>>();
    if new_commands.is_empty() {
        return Vec::new();
    }
    let is_initial = sent.is_empty();
    sent.extend(new_commands.iter().map(|command| command.name.to_string()));
    drop(sent_by_agent);
    let model = tool_use_context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let context_window = crate::utils::context::get_context_window_for_model(&model, &[]);
    let content = crate::tools::skill_tool::prompt::format_commands_within_budget(
        &new_commands,
        context_window,
    );
    let skill_count = new_commands.len();
    vec![attachment_continuation(AttachmentMessage::new(
        Attachment::SkillListing {
            content,
            skill_count,
            is_initial,
        },
    ))]
}

/// Maps to: CC `utils/attachments.ts:2612-2632` `resetSentSkillNames`.
/// Called by `/clear` and skill-cache invalidation owners so a later turn can
/// announce the refreshed registry.
pub(crate) fn reset_sent_skill_names() {
    SENT_SKILL_NAMES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

#[cfg(test)]
fn reset_sent_skill_names_for_test() {
    reset_sent_skill_names();
}

/// Maps to: CC `utils/attachments.ts:2883-2935`
/// `getLSPDiagnosticAttachments`.
fn get_lsp_diagnostic_attachments(
    tool_use_context: &ToolUseContext,
    query_source: &QuerySource,
) -> Vec<AttachmentContinuation> {
    // CC gates these under `mainThreadAttachments`, whose predicate is
    // `attachments.ts:770` `const isMainThread = !toolUseContext.agentId`. This
    // port stands in with the query source (CC's `startsWith('agent:')` shape)
    // and excludes it without draining the pending registry. The two are not
    // interchangeable: `ToolUseContext.agent_id` is also set for in-process
    // teammates (`query.rs:140`/`:4299` from `teammate::get_agent_id()`), whose
    // query source is not an agent source — swapping the operand here would
    // change which turns lose LSP diagnostics, so it is a separate ruling.
    if query_source.is_agent() {
        return Vec::new();
    }

    // LSP diagnostics are only useful if the agent has Bash available to act on
    // them. This mirrors the official preflight before `checkForLSPDiagnostics()`.
    if !tool_use_context
        .tools
        .iter()
        .any(|tool| tool_matches_name(tool, BASH_TOOL_NAME))
    {
        return Vec::new();
    }

    let diagnostic_sets = check_for_lsp_diagnostics();
    if diagnostic_sets.is_empty() {
        return Vec::new();
    }

    let attachments = diagnostic_sets
        .into_iter()
        .map(|set| {
            let files = set.files;
            let id = format!("lsp-diagnostics-{}", uuid::Uuid::new_v4());
            let timestamp = chrono::Utc::now();
            let attachment = AttachmentMessage {
                uuid: id.clone(),
                timestamp,
                attachment: Attachment::Diagnostics {
                    files,
                    is_new: true,
                },
                // Built in-process, so there is no wire form to preserve.
                wire_payload: None,
            };

            AttachmentContinuation {
                renderable_message: RenderableMessage {
                    uuid: id,
                    kind: RenderableMessageKind::Attachment(attachment.attachment.clone()),
                },
                model_message: Message::Attachment(attachment),
            }
        })
        .collect::<Vec<_>>();

    // Official calls this after converting delivered sets, matching the async
    // hook registry cleanup pattern. The Rust registry has already removed sent
    // entries during `check_for_lsp_diagnostics`; this keeps the same boundary.
    clear_all_lsp_diagnostics();

    attachments
}

/// Maps to: CC `utils/attachments.ts:2327-2332` `memoryHeader`.
/// Kept here so resumed attachment normalization uses the producer's header.
pub(crate) fn memory_header(path: &str, mtime_ms: f64) -> String {
    let staleness = crate::memdir::memory_age::memory_freshness_text(mtime_ms);
    if !staleness.is_empty() {
        format!("{staleness}\n\nMemory: {path}:")
    } else {
        format!(
            "Memory (saved {}): {path}:",
            crate::memdir::memory_age::memory_age(mtime_ms)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::lsp::diagnostic_registry::{
        get_pending_lsp_diagnostic_count, register_pending_lsp_diagnostic,
        reset_all_lsp_diagnostic_state,
    };
    use crate::utils::test_env::{EnvVarGuard, PinnedProjectDir, TEST_ENV_LOCK};

    #[test]
    fn memory_header_matches_official_fresh_and_stale_text() {
        let now = chrono::Utc::now().timestamp_millis() as f64;
        let day = 86_400_000.0;
        assert_eq!(
            memory_header("/memory/今天.md", now),
            "Memory (saved today): /memory/今天.md:"
        );
        assert_eq!(
            memory_header("old.md", now - 1.5 * day),
            "Memory (saved yesterday): old.md:"
        );
        assert_eq!(
            memory_header("older.md", now - 2.5 * day),
            "This memory is 2 days old. Memories are point-in-time observations, not live state — claims about code behavior or file:line citations may be outdated. Verify against current code before asserting as fact.\n\nMemory: older.md:"
        );
        assert_eq!(
            memory_header("future.md", now + day),
            "Memory (saved today): future.md:"
        );
    }

    /// CC keeps attachments as plain JS objects: whatever fields a transcript
    /// entry carries survive `JSON.parse` → render → `JSON.stringify`. The
    /// typed enum only declares the fields Cometix reads, so re-serializing it
    /// alone would drop the rest — a payload written by a newer CC, or by a
    /// producer that spread extra keys, would lose them the first time a
    /// session round-trips through Cometix. The transcript payload is
    /// therefore taken from the preserved wire form.
    #[test]
    fn transcript_attachments_round_trip_undeclared_fields_like_official() {
        use crate::types::message::{Attachment, AttachmentMessage};

        let wire = serde_json::json!({
            "type": "skill_listing",
            "content": "- kitty-tui-test: smoke",
            "skillCount": 1,
            "isInitial": false,
            "fieldFromNewerCc": "must survive the round-trip"
        });
        let message = AttachmentMessage::from_wire_payload(
            "attachment-uuid".to_string(),
            chrono::Utc::now(),
            wire.clone(),
        );

        // The typed arm still resolves, so rendering keeps reading real fields.
        assert!(matches!(
            message.attachment,
            Attachment::SkillListing {
                skill_count: 1,
                is_initial: false,
                ..
            }
        ));
        // …and persistence stays byte-for-byte faithful to what CC wrote.
        assert_eq!(message.payload_for_transcript(), wire);
        // The typed projection on its own is where the field is lost — this is
        // what recording used to write.
        let typed_only = serde_json::to_value(&message.attachment).expect("serialize");
        assert!(typed_only.get("fieldFromNewerCc").is_none());

        // Attachments built in-process have no wire form and fall back to the
        // typed projection.
        let live = AttachmentMessage::new(serde_json::json!({
            "type": "compaction_reminder"
        }));
        assert_eq!(live.wire_payload, None);
        assert_eq!(
            live.payload_for_transcript(),
            serde_json::to_value(&live.attachment).expect("serialize")
        );
    }
    use crate::services::lsp::types::{
        Diagnostic, DiagnosticFile, DiagnosticPosition, DiagnosticRange,
    };

    struct McpInstructionGuard(&'static str);

    impl McpInstructionGuard {
        fn set(name: &'static str, instructions: &str) -> Self {
            crate::services::mcp::client::set_mcp_server_instructions_for_test(
                name,
                Some(instructions),
            );
            Self(name)
        }
    }

    impl Drop for McpInstructionGuard {
        fn drop(&mut self) {
            crate::services::mcp::client::set_mcp_server_instructions_for_test(self.0, None);
        }
    }

    /// Batch-D long-tail #2: the nested payloads typed in this batch must
    /// parse into the typed members (not degrade to `Unknown`) for CC-shaped
    /// wire values, and must re-serialize to the identical wire form.
    #[test]
    fn typed_nested_payloads_parse_and_round_trip_cc_wire_shapes() {
        // `file` → content: FileReadToolOutput (FileReadTool.ts:258-268).
        let file = serde_json::json!({
            "type": "file",
            "filename": "/repo/src/lib.rs",
            "displayPath": "src/lib.rs",
            "content": {
                "type": "text",
                "file": {
                    "filePath": "/repo/src/lib.rs",
                    "content": "fn main() {}\n",
                    "numLines": 1,
                    "startLine": 1,
                    "totalLines": 1
                }
            }
        });
        let parsed = Attachment::from_wire(file.clone());
        assert!(matches!(
            &parsed,
            Attachment::File {
                content: crate::tools::file_read_tool::Output::Text { num_lines, .. },
                ..
            } if num_lines.as_u64() == Some(1)
        ));
        assert_eq!(parsed.to_wire(), file);

        // `edited_image_file` image content, dimensions omitted when absent
        // (FileReadTool.ts:270-301).
        let image = serde_json::json!({
            "type": "edited_image_file",
            "filename": "/repo/logo.png",
            "content": {
                "type": "image",
                "file": {"base64": "aW1n", "type": "image/png", "originalSize": 4}
            }
        });
        let parsed = Attachment::from_wire(image.clone());
        assert!(matches!(
            &parsed,
            Attachment::EditedImageFile {
                content: crate::tools::file_read_tool::Output::Image { media_type, .. },
                ..
            } if media_type == "image/png"
        ));
        assert_eq!(parsed.to_wire(), image);

        // `hook_blocking_error` carries the whole HookBlockingError object
        // (utils/hooks.ts:330-333, toolHooks.ts:105-115).
        let blocking = serde_json::json!({
            "type": "hook_blocking_error",
            "hookName": "PostToolUse:Bash",
            "toolUseID": "toolu_1",
            "hookEvent": "PostToolUse",
            "blockingError": {"blockingError": "stop", "command": "lint.sh"}
        });
        let parsed = Attachment::from_wire(blocking.clone());
        assert!(matches!(
            &parsed,
            Attachment::HookBlockingError { blocking_error, .. }
                if blocking_error.blocking_error == "stop" && blocking_error.command == "lint.sh"
        ));
        assert_eq!(parsed.to_wire(), blocking);

        // `nested_memory` → MemoryFileInfo (utils/claudemd.ts:229-243).
        let memory = serde_json::json!({
            "type": "nested_memory",
            "path": "/repo/docs/CLAUDE.md",
            "displayPath": "docs/CLAUDE.md",
            "content": {
                "path": "/repo/docs/CLAUDE.md",
                "type": "Project",
                "content": "notes",
                "contentDiffersFromDisk": true,
                "rawContent": "<!-- raw -->notes"
            }
        });
        let parsed = Attachment::from_wire(memory.clone());
        assert!(matches!(
            &parsed,
            Attachment::NestedMemory { content, .. }
                if content.memory_type == "Project"
                    && content.raw_content.as_deref() == Some("<!-- raw -->notes")
        ));
        assert_eq!(parsed.to_wire(), memory);

        // `todo_reminder` → TodoList (utils/todo/types.ts:8-18) and
        // `task_reminder` → Task[] (utils/tasks.ts:76-89).
        let todos = serde_json::json!({
            "type": "todo_reminder",
            "itemCount": 1,
            "content": [{"content": "port", "status": "in_progress", "activeForm": "Porting"}]
        });
        let parsed = Attachment::from_wire(todos.clone());
        assert!(matches!(
            &parsed,
            Attachment::TodoReminder { content, .. }
                if content.len() == 1 && content[0].active_form == "Porting"
        ));
        assert_eq!(parsed.to_wire(), todos);
        let tasks = serde_json::json!({
            "type": "task_reminder",
            "itemCount": 1,
            "content": [{
                "id": "1",
                "subject": "port",
                "description": "port batch D",
                "status": "pending",
                "blocks": [],
                "blockedBy": []
            }]
        });
        let parsed = Attachment::from_wire(tasks.clone());
        assert!(matches!(
            &parsed,
            Attachment::TaskReminder { content, .. }
                if content.len() == 1 && content[0].subject == "port"
        ));
        assert_eq!(parsed.to_wire(), tasks);

        // Totality pin: a `file` whose content misses a required schema field
        // must NOT drop — it rides `Attachment::Unknown` losslessly (the D2
        // contract; CC's loose declaration, types/message.ts:112).
        let malformed = serde_json::json!({
            "type": "file",
            "filename": "/repo/src/lib.rs",
            "content": {"type": "text", "file": {"content": "fn main() {}\n"}}
        });
        let parsed = Attachment::from_wire(malformed.clone());
        assert!(matches!(&parsed, Attachment::Unknown(value) if *value == malformed));
        assert_eq!(parsed.wire_type(), "file");
        assert_eq!(parsed.to_wire(), malformed);
    }

    #[test]
    fn at_mention_extract_and_line_parser_match_official_quoted_and_fragment_rules() {
        assert_eq!(
            extract_at_mentioned_files(
                "read @src/main.rs#L10-20 and @\"docs/file with spaces.md\" then @src/main.rs#L10-20 @\"reviewer (agent)\""
            ),
            vec![
                "docs/file with spaces.md".to_string(),
                "src/main.rs#L10-20".to_string(),
            ]
        );
        assert_eq!(
            parse_at_mentioned_file_lines("src/main.rs#L10-20"),
            AtMentionedFileLines {
                filename: "src/main.rs".to_string(),
                line_start: Some(10),
                line_end: Some(20),
            }
        );
        assert_eq!(
            parse_at_mentioned_file_lines("README.md#overview"),
            AtMentionedFileLines {
                filename: "README.md".to_string(),
                line_start: None,
                line_end: None,
            }
        );
        assert_eq!(
            parse_at_mentioned_file_lines("README.md#L0"),
            AtMentionedFileLines {
                filename: "README.md".to_string(),
                line_start: Some(0),
                line_end: Some(0),
            }
        );
        assert_eq!(
            extract_mcp_resource_mentions(
                "use @docs:file:///guide and @docs:file:///guide then @other:resource/path"
            ),
            vec![
                "docs:file:///guide".to_string(),
                "other:resource/path".to_string(),
            ]
        );
        assert_eq!(
            extract_agent_mentions(
                "ask @agent-code-reviewer and @\"plugin:planner (agent)\" then @agent-code-reviewer"
            ),
            vec![
                "plugin:planner".to_string(),
                "agent-code-reviewer".to_string(),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn user_input_attachment_phase_keeps_file_then_agent_producer_order() {
        let root = std::env::temp_dir().join(format!(
            "cometix-user-attachment-order-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "note\n").unwrap();
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "reviewer",
            "Review changes",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        let definitions = crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
            active_agents: vec![agent.clone()],
            all_agents: vec![agent],
            failed_files: Vec::new(),
            allowed_agent_types: None,
        };
        let mut context = ToolUseContext::default()
            .with_cwd_override(Some(root.clone()))
            .with_agent_definitions(std::sync::Arc::new(definitions));
        let attachments =
            process_user_input_attachments("inspect @note.txt with @agent-reviewer", &mut context)
                .await;
        assert_eq!(attachments.len(), 2);
        assert!(matches!(
            &attachments[0].model_message,
            Message::Attachment(attachment) if attachment.attachment_type() == "file"
        ));
        assert!(matches!(
            &attachments[1].model_message,
            Message::Attachment(attachment)
                if attachment.attachment_type() == "agent_mention"
                    && attachment.attachment.to_wire()["agentType"] == "reviewer"
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn slow_at_mention_sibling_keeps_completed_file_attachment_and_state() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = std::env::temp_dir().join(format!(
            "cometix-at-mention-timeout-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "completed\n").unwrap();
        let fifo = root.join("z.pipe");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let fifo_for_writer = fifo.clone();
        let writer = std::thread::spawn(move || {
            use std::os::unix::fs::OpenOptionsExt as _;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                if let Ok(_file) = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&fifo_for_writer)
                {
                    std::thread::sleep(std::time::Duration::from_millis(1_250));
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });

        let mut context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        let attachments =
            process_user_input_attachments("inspect @a.txt and @z.pipe", &mut context).await;

        assert_eq!(attachments.len(), 1);
        assert!(matches!(
            &attachments[0].model_message,
            Message::Attachment(attachment)
                if attachment.attachment_type() == "file"
                    && attachment.attachment.to_wire()["filename"]
                        .as_str()
                        .is_some_and(|path| path.ends_with("a.txt"))
        ));
        assert!(
            context
                .read_file_state
                .snapshot()
                .into_iter()
                .any(|entry| entry.path.ends_with("a.txt"))
        );
        writer.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compact_file_restoration_keeps_file_unchanged_attachment() {
        let root = std::env::temp_dir().join(format!(
            "cometix-compact-file-unchanged-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("same.txt");
        std::fs::write(&path, "same content\n").unwrap();
        let mut context = ToolUseContext::default().with_cwd_override(Some(root.clone()));
        let limits = crate::tools::file_read_tool::limits::get_default_file_reading_limits();

        let first = generate_file_attachment(
            path.to_string_lossy().as_ref(),
            &mut context,
            FileAttachmentMode::Compact,
            GenerateFileAttachmentOptions::default(),
            limits,
        )
        .await
        .expect("initial compact attachment");
        assert_eq!(first.attachment_type(), "file");

        let unchanged = generate_file_attachment(
            path.to_string_lossy().as_ref(),
            &mut context,
            FileAttachmentMode::Compact,
            GenerateFileAttachmentOptions::default(),
            limits,
        )
        .await
        .expect("unchanged compact attachment");
        assert_eq!(unchanged.attachment_type(), "file");
        assert_eq!(
            unchanged.attachment.to_wire()["content"]["type"],
            "file_unchanged"
        );
        assert_eq!(
            unchanged.attachment.to_wire()["content"]["file"]["filePath"],
            path.to_string_lossy().as_ref()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn at_mention_file_and_directory_flow_through_read_attachment_owner() {
        let root = std::env::temp_dir().join(format!(
            "cometix-at-mention-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let source = root.join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("main.rs"), "one\ntwo\nthree\nfour\n").unwrap();
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };
        let attachments =
            process_at_mentioned_files("inspect @src/main.rs#L2-3 and @src", &mut context).await;
        assert_eq!(attachments.len(), 2);
        let file = match &attachments[0].model_message {
            Message::Attachment(attachment) => attachment,
            other => panic!("unexpected file attachment: {other:?}"),
        };
        assert_eq!(file.attachment_type(), "file");
        assert_eq!(
            file.attachment.to_wire()["content"]["file"]["content"],
            "two\nthree"
        );
        let directory = match &attachments[1].model_message {
            Message::Attachment(attachment) => attachment,
            other => panic!("unexpected directory attachment: {other:?}"),
        };
        assert_eq!(directory.attachment_type(), "directory");
        assert_eq!(directory.attachment.to_wire()["content"], "main.rs");
        assert!(context.read_file_state.snapshot().into_iter().any(|entry| {
            entry.path.ends_with("src/main.rs")
                && entry.offset == Some(serde_json::json!(2))
                && entry.limit == Some(serde_json::json!(2))
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn transformed_nested_memory_is_cached_as_partial_raw_disk_view() {
        let root = std::env::temp_dir().join(format!(
            "cometix-nested-memory-partial-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let rules = root.join(".claude/rules");
        let target = root.join("src/main.rs");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "fn main() {}\n").unwrap();
        let rule = rules.join("rust.md");
        let raw = "---\npaths: src/**/*.rs\n---\nrust instruction";
        std::fs::write(&rule, raw).unwrap();
        let mut processed = std::collections::HashSet::new();
        let files = crate::utils::claudemd::get_memory_files_for_nested_directory(
            &root,
            &target,
            &root,
            &mut processed,
        );
        assert_eq!(files.len(), 1);
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };

        let attachments = memory_files_to_attachments(files, &mut context, Some(&target));
        assert_eq!(attachments.len(), 1);
        let canonical_rule = std::fs::canonicalize(&rule).unwrap();
        let state = context
            .read_file_state
            .snapshot()
            .into_iter()
            .find(|entry| {
                std::fs::canonicalize(&entry.path)
                    .is_ok_and(|candidate| candidate == canonical_rule)
            })
            .expect("nested memory state");
        assert!(state.is_partial_view);
        assert_eq!(state.content.as_deref(), Some(raw));
        let Message::Attachment(attachment) = &attachments[0].model_message else {
            panic!("expected nested memory attachment")
        };
        assert_eq!(
            attachment.attachment.to_wire()["content"]["content"],
            "rust instruction"
        );
        assert_eq!(
            attachment.attachment.to_wire()["content"]["contentDiffersFromDisk"],
            true
        );
        assert_eq!(
            attachment.attachment.to_wire()["content"]["rawContent"],
            raw
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn at_mention_token_fallback_keeps_truncated_read_state_range() {
        let root = std::env::temp_dir().join(format!(
            "cometix-at-mention-token-fallback-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("dense.json");
        let content = (0..3_000)
            .map(|index| format!("{{\"dense\":{index:010}}}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, content).unwrap();
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };

        let attachments = process_at_mentioned_files("inspect @dense.json", &mut context).await;
        assert_eq!(attachments.len(), 1);
        let Message::Attachment(attachment) = &attachments[0].model_message else {
            panic!("expected file attachment")
        };
        assert_eq!(attachment.attachment.to_wire()["truncated"], true);
        let state = context
            .read_file_state
            .snapshot()
            .into_iter()
            .find(|entry| entry.path == path.display().to_string())
            .expect("truncated Read state");
        assert_eq!(state.offset, Some(serde_json::json!(1)));
        assert_eq!(state.limit, Some(serde_json::json!(2_000)));
        assert_eq!(
            state
                .content
                .as_deref()
                .unwrap_or_default()
                .split('\n')
                .count(),
            2_000
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn at_mention_read_discovers_nested_dynamic_skill_directory() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::skills::load_skills_dir::clear_dynamic_skills();
        struct DynamicSkillsRestore;
        impl Drop for DynamicSkillsRestore {
            fn drop(&mut self) {
                crate::skills::load_skills_dir::clear_dynamic_skills();
            }
        }
        let _restore = DynamicSkillsRestore;
        let root = std::env::temp_dir().join(format!(
            "cometix-at-mention-dynamic-skill-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skill_dir = root.join("nested/.claude/skills");
        let source_dir = root.join("nested/src");
        std::fs::create_dir_all(skill_dir.join("alpha")).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(skill_dir.join("alpha/SKILL.md"), "# Alpha").unwrap();
        std::fs::write(source_dir.join("value.txt"), "value\n").unwrap();
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };

        let attachments =
            process_at_mentioned_files("inspect @nested/src/value.txt", &mut context).await;
        assert_eq!(attachments.len(), 1);
        assert!(
            context
                .dynamic_skill_dir_triggers
                .as_ref()
                .is_some_and(|triggers| triggers.contains(&skill_dir.display().to_string()))
        );
        let dynamic = get_dynamic_skill_attachments(&mut context).await;
        assert_eq!(dynamic.len(), 1);
        assert!(matches!(
            &dynamic[0].renderable_message.kind,
            RenderableMessageKind::Attachment(Attachment::DynamicSkill {
                skill_names,
                ..
            }) if skill_names == &["alpha"]
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_at_mention_still_discovers_nested_dynamic_skill_directory() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::skills::load_skills_dir::clear_dynamic_skills();
        struct DynamicSkillsRestore;
        impl Drop for DynamicSkillsRestore {
            fn drop(&mut self) {
                crate::skills::load_skills_dir::clear_dynamic_skills();
            }
        }
        let _restore = DynamicSkillsRestore;
        let root = std::env::temp_dir().join(format!(
            "cometix-at-mention-missing-dynamic-skill-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skill_dir = root.join("nested/.claude/skills");
        std::fs::create_dir_all(skill_dir.join("alpha")).unwrap();
        std::fs::create_dir_all(root.join("nested/src")).unwrap();
        std::fs::write(skill_dir.join("alpha/SKILL.md"), "# Alpha").unwrap();
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };

        let attachments =
            process_at_mentioned_files("inspect @nested/src/missing.txt", &mut context).await;
        assert!(attachments.is_empty());
        assert!(
            context
                .dynamic_skill_dir_triggers
                .as_ref()
                .is_some_and(|triggers| triggers.contains(&skill_dir.display().to_string()))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nested_live_iteration_accepts_insertion_during_source_await() {
        // At-mention reads resolve against the project dir.
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.join(format!(
            ".cometix-nested-race-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let rules = root.join(".claude/rules");
        let joined_target = root.join("src/main.rs");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::create_dir_all(joined_target.parent().unwrap()).unwrap();
        std::fs::write(&joined_target, "fn main() {}\n").unwrap();
        std::fs::write(
            rules.join("joined.md"),
            "---\npaths: src/**/*.rs\n---\njoined instruction",
        )
        .unwrap();
        let mut context = ToolUseContext {
            cwd_override: Some(cwd.clone()),
            ..ToolUseContext::default()
        };
        let triggers = context
            .nested_memory_attachment_triggers
            .as_ref()
            .unwrap()
            .clone();
        triggers.add(cwd.join("Cargo.toml").display().to_string());

        let mut traversal = Box::pin(get_nested_memory_attachments(&mut context));
        assert!(matches!(
            futures::poll!(&mut traversal),
            std::task::Poll::Pending
        ));
        triggers.add(joined_target.display().to_string());
        let attachments = traversal.await;

        assert!(attachments.iter().any(|attachment| {
            matches!(
                &attachment.model_message,
                Message::Attachment(message)
                    if message.attachment_type() == "nested_memory"
                        && message.attachment.to_wire()["content"]["content"] == "joined instruction"
            )
        }));
        assert!(triggers.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_snapshot_clears_insertion_that_arrives_during_source_await() {
        let root = std::env::temp_dir().join(format!(
            "cometix-dynamic-race-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let first = root.join("first");
        let late = root.join("late");
        std::fs::create_dir_all(first.join("alpha")).unwrap();
        std::fs::create_dir_all(late.join("beta")).unwrap();
        std::fs::write(first.join("alpha/SKILL.md"), "# Alpha").unwrap();
        std::fs::write(late.join("beta/SKILL.md"), "# Beta").unwrap();
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };
        let triggers = context.dynamic_skill_dir_triggers.as_ref().unwrap().clone();
        triggers.add(first.display().to_string());

        let mut traversal = Box::pin(get_dynamic_skill_attachments(&mut context));
        assert!(matches!(
            futures::poll!(&mut traversal),
            std::task::Poll::Pending
        ));
        triggers.add(late.display().to_string());
        let attachments = traversal.await;

        assert_eq!(attachments.len(), 1);
        let Message::Attachment(message) = &attachments[0].model_message else {
            panic!("expected dynamic skill attachment")
        };
        assert_eq!(
            message.attachment.to_wire()["skillNames"],
            serde_json::json!(["alpha"])
        );
        assert!(triggers.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn changed_image_uses_second_helper_without_replaying_read_effects() {
        const TINY_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D,
            0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let root = std::env::temp_dir().join(format!(
            "cometix-changed-image-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("watched.png");
        std::fs::write(&path, TINY_PNG).unwrap();
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            read_file_state: crate::tool::SharedFileStateCache::from_entries(vec![
                crate::utils::query_helpers::ReadFileStateEntry {
                    path: path.display().to_string(),
                    content: Some(String::new()),
                    timestamp_ms: Some(0),
                    offset: None,
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Write,
                },
            ]),
            ..ToolUseContext::default()
        };

        let attachments = get_changed_files(&mut context).await;
        assert_eq!(attachments.len(), 1);
        let Message::Attachment(attachment) = &attachments[0].model_message else {
            panic!("expected edited image attachment")
        };
        assert_eq!(attachment.attachment_type(), "edited_image_file");
        assert_eq!(
            attachment.attachment.to_wire()["content"]["file"]["type"],
            "image/png"
        );
        assert_eq!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .snapshot(),
            vec![path.display().to_string()]
        );
        assert_eq!(context.read_file_state.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn changed_full_file_state_emits_edited_text_reminder_and_refreshes_cache() {
        let root = std::env::temp_dir().join(format!(
            "cometix-changed-file-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("watched.txt");
        std::fs::write(&path, "new content\n").unwrap();
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            read_file_state: crate::tool::SharedFileStateCache::from_entries(vec![
                crate::utils::query_helpers::ReadFileStateEntry {
                    path: path.display().to_string(),
                    content: Some("old content\n".to_string()),
                    timestamp_ms: Some(0),
                    offset: None,
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Write,
                },
            ]),
            ..ToolUseContext::default()
        };

        let attachments = get_changed_files(&mut context).await;
        assert_eq!(attachments.len(), 1);
        let Message::Attachment(attachment) = &attachments[0].model_message else {
            panic!("expected edited text attachment")
        };
        assert_eq!(attachment.attachment_type(), "edited_text_file");
        let normalized = crate::utils::messages::normalize_attachment_for_api(attachment, None);
        assert!(matches!(
            &normalized[0].content[0],
            crate::types::message::UserContent::MetaText(text)
                if text.contains("was modified") && text.contains("new content")
        ));
        let refreshed = context
            .read_file_state
            .snapshot()
            .into_iter()
            .find(|entry| entry.path == path.display().to_string())
            .expect("refreshed state");
        assert_eq!(refreshed.content.as_deref(), Some("new content\n"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skill_listing_is_agent_scoped_and_announces_dynamic_deltas() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _node_env = EnvVarGuard::unset("NODE_ENV");
        struct SentSkillNamesRestore(
            std::collections::HashMap<String, std::collections::HashSet<String>>,
        );
        impl Drop for SentSkillNamesRestore {
            fn drop(&mut self) {
                *SENT_SKILL_NAMES
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = self.0.clone();
            }
        }
        let _restore = SentSkillNamesRestore(
            SENT_SKILL_NAMES
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
        );
        reset_sent_skill_names_for_test();
        let make_command = |name: &str| {
            crate::commands::Command::from_mcp_prompt(
                crate::services::mcp::client::McpPromptCommandSnapshot {
                    name: name.to_string(),
                    description: format!("Use {name}"),
                    has_user_specified_description: true,
                    user_facing_name: name.to_string(),
                    arg_names: Vec::new(),
                    source: "mcp",
                },
            )
        };
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::skill_tool::skill_tool_schema()];
        context.mcp_state.commands = vec![make_command("mcp:first")];

        let initial = get_skill_listing_attachments(&mut context);
        assert_eq!(initial.len(), 1);
        let Message::Attachment(initial) = &initial[0].model_message else {
            panic!("expected skill listing")
        };
        assert!(
            initial.attachment.to_wire()["content"]
                .as_str()
                .is_some_and(|content| content.contains("mcp:first"))
        );
        assert!(get_skill_listing_attachments(&mut context).is_empty());

        context.mcp_state.commands.push(make_command("mcp:second"));
        let delta = get_skill_listing_attachments(&mut context);
        assert_eq!(delta.len(), 1);
        let Message::Attachment(delta) = &delta[0].model_message else {
            panic!("expected skill delta")
        };
        assert_eq!(delta.attachment.to_wire()["skillCount"], 1);
        assert_eq!(delta.attachment.to_wire()["isInitial"], false);
        assert!(
            delta.attachment.to_wire()["content"]
                .as_str()
                .is_some_and(|content| content.contains("mcp:second"))
        );
    }

    #[test]
    fn skill_listing_prefers_live_app_state_mcp_commands_over_snapshot() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        struct SentSkillNamesRestore(
            std::collections::HashMap<String, std::collections::HashSet<String>>,
        );
        impl Drop for SentSkillNamesRestore {
            fn drop(&mut self) {
                *SENT_SKILL_NAMES
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = self.0.clone();
            }
        }
        let _restore = SentSkillNamesRestore(
            SENT_SKILL_NAMES
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
        );
        reset_sent_skill_names_for_test();

        let make_command = |name: &str| {
            crate::commands::Command::from_mcp_prompt(
                crate::services::mcp::client::McpPromptCommandSnapshot {
                    name: name.to_string(),
                    description: format!("Use {name}"),
                    has_user_specified_description: true,
                    user_facing_name: name.to_string(),
                    arg_names: Vec::new(),
                    source: "mcp",
                },
            )
        };
        let mut live_state = crate::state::app_state_store::AppState::default();
        live_state.mcp = std::sync::Arc::new(crate::state::app_state_store::McpState {
            commands: vec![make_command("mcp:live")],
            ..Default::default()
        });
        let store = crate::state::store::AppStore::new(live_state, None);
        let mut context = ToolUseContext::default().with_app_store(store);
        // Simulate a stale per-turn snapshot after a prompts/list_changed store
        // update. The source reads getAppState() at this point.
        context.mcp_state.commands = vec![make_command("mcp:stale")];
        context.tools = vec![crate::tools::skill_tool::skill_tool_schema()];

        let attachments = get_skill_listing_attachments(&mut context);
        assert_eq!(attachments.len(), 1);
        let Message::Attachment(message) = &attachments[0].model_message else {
            panic!("expected skill listing");
        };
        let wire = message.attachment.to_wire();
        let content = wire["content"].as_str().expect("skill listing content");
        assert!(content.contains("mcp:live"));
        assert!(!content.contains("mcp:stale"));
    }

    fn diag(message: &str) -> Diagnostic {
        Diagnostic {
            message: message.to_string(),
            severity: "Error".to_string(),
            range: DiagnosticRange {
                start: DiagnosticPosition {
                    line: 2,
                    character: 4,
                },
                end: DiagnosticPosition {
                    line: 2,
                    character: 9,
                },
            },
            source: Some("rust-analyzer".to_string()),
            code: Some("E0425".to_string()),
        }
    }

    fn file(uri: &str, diagnostics: Vec<Diagnostic>) -> DiagnosticFile {
        DiagnosticFile {
            uri: uri.to_string(),
            diagnostics,
        }
    }

    /// CC `getQueuedCommandAttachments` is a pure projection over the
    /// caller-drained snapshot (attachments.ts:1046-1083); the payload carries
    /// `source_uuid`/`commandMode`/`isMeta` (:1075-1079).
    #[tokio::test]
    async fn queued_command_snapshot_projects_to_attachment_with_payload_fields() {
        let snapshot = vec![
            crate::utils::message_queue_manager::QueuedCommand {
                value: "<task-notification><status>completed</status></task-notification>"
                    .to_string(),
                pre_expansion_value: None,
                pasted_contents: Default::default(),
                mode: "task-notification".to_string(),
                priority: crate::utils::message_queue_manager::QueuePriority::Later,
                agent_id: None,
                is_meta: true,
                uuid: Some("queued-1".to_string()),
                skip_slash_commands: true,
            },
            // A non-inline mode in the snapshot is skipped by the
            // INLINE_NOTIFICATION_MODES filter (CC :1057-1059).
            crate::utils::message_queue_manager::QueuedCommand {
                value: "ls".to_string(),
                pre_expansion_value: None,
                pasted_contents: Default::default(),
                mode: "bash".to_string(),
                priority: crate::utils::message_queue_manager::QueuePriority::Next,
                agent_id: None,
                is_meta: false,
                uuid: None,
                skip_slash_commands: false,
            },
        ];

        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut ToolUseContext::default(),
            messages: &[],
            query_source: &QuerySource::Prompt,
            queued_commands: snapshot,
        })
        .await;

        let queued: Vec<_> = attachments
            .iter()
            .filter(|attachment| {
                matches!(
                    attachment.renderable_message.kind,
                    RenderableMessageKind::Attachment(Attachment::QueuedCommand { .. })
                )
            })
            .collect();
        assert_eq!(queued.len(), 1, "bash-mode command must not project");
        match &queued[0].model_message {
            Message::Attachment(attachment) => {
                assert_eq!(attachment.attachment_type(), "queued_command");
                match &attachment.attachment {
                    Attachment::QueuedCommand {
                        prompt,
                        source_uuid,
                        command_mode,
                        is_meta,
                        ..
                    } => {
                        assert!(
                            prompt
                                .as_str()
                                .is_some_and(|prompt| prompt.contains("<task-notification>"))
                        );
                        assert_eq!(source_uuid.as_deref(), Some("queued-1"));
                        assert_eq!(command_mode.as_deref(), Some("task-notification"));
                        assert_eq!(*is_meta, Some(true));
                    }
                    other => panic!("unexpected queued command payload: {other:?}"),
                }
            }
            other => panic!("unexpected queued command model message: {other:?}"),
        }
    }

    /// CC `isMeta: _.isMeta` (:1079) leaves the field undefined when the
    /// enqueue site never set it — a `false` projects to the absent field.
    #[tokio::test]
    async fn queued_command_without_is_meta_omits_the_field() {
        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut ToolUseContext::default(),
            messages: &[],
            query_source: &QuerySource::Prompt,
            queued_commands: vec![crate::utils::message_queue_manager::QueuedCommand::new(
                "hello there",
                "prompt",
            )],
        })
        .await;
        let queued = attachments
            .iter()
            .find_map(|attachment| match &attachment.model_message {
                Message::Attachment(message) => match &message.attachment {
                    Attachment::QueuedCommand { is_meta, .. } => Some(*is_meta),
                    _ => None,
                },
                _ => None,
            })
            .expect("queued command attachment");
        assert_eq!(queued, None);
    }

    #[tokio::test]
    async fn dynamic_skill_attachment_lists_discovered_skills_and_drains_triggers() {
        let root = std::env::temp_dir().join(format!(
            "cometix-dynamic-skill-attachment-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skills_dir = root.join("nested/.claude/skills");
        std::fs::create_dir_all(skills_dir.join("alpha")).unwrap();
        std::fs::write(skills_dir.join("alpha/SKILL.md"), "# Alpha").unwrap();
        let mut context = ToolUseContext::default();
        context.cwd_override = Some(root.clone());
        context
            .dynamic_skill_dir_triggers
            .as_ref()
            .unwrap()
            .add(skills_dir.display().to_string());

        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut context,
            messages: &[],
            query_source: &QuerySource::Prompt,
            queued_commands: Vec::new(),
        })
        .await;
        assert!(
            context
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .is_empty()
        );
        let dynamic_attachments = attachments
            .iter()
            .filter(|attachment| {
                matches!(
                    attachment.renderable_message.kind,
                    RenderableMessageKind::Attachment(Attachment::DynamicSkill { .. })
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(dynamic_attachments.len(), 1);
        assert!(matches!(
            &dynamic_attachments[0].renderable_message.kind,
            RenderableMessageKind::Attachment(Attachment::DynamicSkill {
                skill_names,
                display_path,
                ..
            }) if skill_names == &["alpha"] && display_path == "nested/.claude/skills"
        ));
        let Message::Attachment(model) = &dynamic_attachments[0].model_message else {
            panic!("expected local dynamic_skill attachment")
        };
        assert_eq!(model.attachment_type(), "dynamic_skill");
        assert!(crate::utils::messages::normalize_attachment_for_api(model, None).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dynamic_skill_display_path_matches_node_relative_outside_cwd() {
        assert_eq!(
            relative_display_path(
                std::path::Path::new("/tmp/project/worktree"),
                std::path::Path::new("/tmp/project/shared/.claude/skills"),
            ),
            "../shared/.claude/skills"
        );
    }

    #[test]
    fn plan_mode_attachments_follow_official_turn_throttle_and_cycle() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let old_exited = crate::bootstrap::state::has_exited_plan_mode_in_session();
        let old_needs_exit = crate::bootstrap::state::needs_plan_mode_exit_attachment();
        struct PlanStateRestore(bool, bool);
        impl Drop for PlanStateRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_has_exited_plan_mode(self.0);
                crate::bootstrap::state::set_needs_plan_mode_exit_attachment(self.1);
            }
        }
        let _restore = PlanStateRestore(old_exited, old_needs_exit);
        crate::bootstrap::state::set_has_exited_plan_mode(false);
        crate::bootstrap::state::set_needs_plan_mode_exit_attachment(false);

        let mut context = ToolUseContext::default();
        context.tool_permission_context.mode = crate::types::permissions::PermissionMode::Plan;
        let initial = get_plan_mode_attachments(&[], &context);
        assert_eq!(initial.len(), 1);
        let Message::Attachment(initial_payload) = &initial[0].model_message else {
            panic!("expected plan attachment");
        };
        assert_eq!(initial_payload.attachment.to_wire()["type"], "plan_mode");
        assert_eq!(initial_payload.attachment.to_wire()["reminderType"], "full");

        let mut history = vec![initial[0].model_message.clone()];
        for index in 0..4 {
            history.push(Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(format!(
                    "turn {index}"
                ))],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }));
        }
        assert!(get_plan_mode_attachments(&history, &context).is_empty());
        history.push(Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "fifth turn".to_string(),
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
        }));
        let next = get_plan_mode_attachments(&history, &context);
        let Message::Attachment(next_payload) = &next[0].model_message else {
            panic!("expected throttled plan attachment");
        };
        assert_eq!(next_payload.attachment.to_wire()["reminderType"], "sparse");
    }

    #[tokio::test]
    async fn dynamic_skill_precedes_main_thread_reminders_in_async_attachment_order() {
        let root = std::env::temp_dir().join(format!(
            "cometix-dynamic-skill-order-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skills_dir = root.join("nested/.claude/skills");
        std::fs::create_dir_all(skills_dir.join("alpha")).unwrap();
        std::fs::write(skills_dir.join("alpha/SKILL.md"), "# Alpha").unwrap();
        let mut context = ToolUseContext::default();
        context.cwd_override = Some(root.clone());
        context
            .dynamic_skill_dir_triggers
            .as_ref()
            .unwrap()
            .add(skills_dir.display().to_string());
        context.critical_system_reminder_experimental = Some("remember".to_string());

        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut context,
            messages: &[],
            query_source: &QuerySource::Prompt,
            queued_commands: Vec::new(),
        })
        .await;
        let dynamic_index = attachments
            .iter()
            .position(|attachment| {
                matches!(
                    attachment.renderable_message.kind,
                    RenderableMessageKind::Attachment(Attachment::DynamicSkill { .. })
                )
            })
            .expect("dynamic skill attachment");
        let reminder_index = attachments
            .iter()
            .position(|attachment| {
                matches!(
                    attachment.renderable_message.kind,
                    RenderableMessageKind::Attachment(Attachment::CriticalSystemReminder { .. })
                )
            })
            .expect("critical reminder attachment");
        assert!(dynamic_index < reminder_index);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn critical_system_reminder_attachment_matches_official_tool_context_field() {
        let mut context = ToolUseContext::default()
            .with_critical_system_reminder_experimental(Some("Verify every claim.".to_string()));

        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut context,
            messages: &[],
            query_source: &QuerySource::Agent,
            queued_commands: Vec::new(),
        })
        .await;

        let reminder = attachments
            .iter()
            .find(|attachment| {
                matches!(
                    attachment.renderable_message.kind,
                    RenderableMessageKind::Attachment(Attachment::CriticalSystemReminder { .. })
                )
            })
            .expect("critical reminder attachment");
        match &reminder.renderable_message.kind {
            RenderableMessageKind::Attachment(Attachment::CriticalSystemReminder { content }) => {
                assert_eq!(content, "Verify every claim.")
            }
            other => panic!("unexpected critical reminder transcript: {other:?}"),
        }
        match &reminder.model_message {
            Message::Attachment(attachment) => {
                assert_eq!(attachment.attachment_type(), "critical_system_reminder");
                assert!(matches!(
                    &attachment.attachment,
                    Attachment::CriticalSystemReminder { content }
                        if content == "Verify every claim."
                ));
            }
            other => panic!("unexpected critical reminder model message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn agent_pending_message_attachments_drain_agent_task_queue() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
        );
        assert_eq!(
            crate::tasks::local_agent_task::queue_pending_message(&task_id, "continue with tests"),
            crate::tasks::local_agent_task::QueuePendingMessageResult::Queued
        );
        let mut context = ToolUseContext::default().with_agent_id(Some(task_id.clone()));
        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut context,
            messages: &[],
            query_source: &QuerySource::Agent,
            queued_commands: Vec::new(),
        })
        .await;
        let queued = attachments
            .iter()
            .find(|attachment| {
                matches!(
                    attachment.renderable_message.kind,
                    RenderableMessageKind::Attachment(Attachment::QueuedCommand { .. })
                )
            })
            .expect("agent pending queued command");
        match &queued.model_message {
            Message::Attachment(attachment) => {
                assert_eq!(attachment.attachment_type(), "queued_command");
                assert!(matches!(
                    &attachment.attachment,
                    Attachment::QueuedCommand { prompt, .. }
                        if prompt.as_str() == Some("continue with tests")
                ));
            }
            other => panic!("unexpected agent pending model message: {other:?}"),
        }
        assert!(crate::tasks::local_agent_task::drain_pending_messages(&task_id).is_empty());
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
    }

    /// `get_attachments` is a pure consumer of the caller's snapshot — it
    /// must never reach into the global command queue itself (the drain and
    /// its thread gating live in query.rs `drain_queued_commands_snapshot`,
    /// CC query.ts:1566-1578; CC turn-0 callers pass `[]`).
    #[tokio::test]
    async fn get_attachments_never_touches_the_global_command_queue() {
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::message_queue_manager::enqueue_pending_notification(
            crate::utils::message_queue_manager::QueuedCommand {
                value: "<task-notification/>".to_string(),
                pre_expansion_value: None,
                pasted_contents: Default::default(),
                mode: "task-notification".to_string(),
                priority: crate::utils::message_queue_manager::QueuePriority::Later,
                agent_id: None,
                is_meta: true,
                uuid: None,
                skip_slash_commands: true,
            },
        );
        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut ToolUseContext::default(),
            messages: &[],
            query_source: &QuerySource::Prompt,
            queued_commands: Vec::new(),
        })
        .await;
        assert!(!attachments.iter().any(|attachment| matches!(
            attachment.renderable_message.kind,
            RenderableMessageKind::Attachment(Attachment::QueuedCommand { .. })
        )));
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            1
        );
        crate::utils::message_queue_manager::clear_command_queue();
    }

    #[tokio::test]
    async fn lsp_diagnostic_attachments_require_bash_without_draining_registry() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();
        register_pending_lsp_diagnostic("rust", vec![file("/tmp/a.rs", vec![diag("broken")])]);

        let mut context = ToolUseContext {
            tools: Vec::new(),
            ..ToolUseContext::default()
        };
        let messages = Vec::new();
        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut context,
            messages: &messages,
            query_source: &QuerySource::Prompt,
            queued_commands: Vec::new(),
        })
        .await;

        assert!(attachments.is_empty());
        assert_eq!(get_pending_lsp_diagnostic_count(), 1);
        reset_all_lsp_diagnostic_state();
    }

    #[tokio::test]
    async fn lsp_diagnostic_attachments_emit_renderable_and_model_messages() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();
        register_pending_lsp_diagnostic(
            "rust",
            vec![file("/tmp/a.rs", vec![diag("cannot find value")])],
        );

        let mut context = ToolUseContext {
            tools: vec![crate::tools::bash_tool::bash_tool_schema()],
            ..ToolUseContext::default()
        };
        let messages = Vec::new();
        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut context,
            messages: &messages,
            query_source: &QuerySource::Prompt,
            queued_commands: Vec::new(),
        })
        .await;

        assert_eq!(attachments.len(), 1);
        match &attachments[0].renderable_message.kind {
            RenderableMessageKind::Attachment(Attachment::Diagnostics { files, is_new }) => {
                // The pre-baked display summary is gone (batch D2): the
                // renderer derives it from `files` at render time.
                assert!(is_new);
                assert_eq!(files[0].diagnostics[0].message, "cannot find value");
            }
            other => panic!("unexpected renderable attachment: {other:?}"),
        }

        match &attachments[0].model_message {
            Message::Attachment(attachment) => {
                assert_eq!(attachment.attachment_type(), "diagnostics");
                assert_eq!(
                    attachment.attachment.to_wire()["files"][0]["diagnostics"][0]["message"],
                    "cannot find value"
                );
                // The model value carries no pre-baked display summary
                // (batch D2: the renderer derives it from `files`).
                assert!(attachment.attachment.to_wire().get("summary").is_none());
            }
            other => panic!("unexpected model attachment: {other:?}"),
        }
        assert_eq!(get_pending_lsp_diagnostic_count(), 0);
        reset_all_lsp_diagnostic_state();
    }

    /// CC `attachments.ts:1503-1521` reads `allowedAgentTypes` straight off
    /// `toolUseContext.options.agentDefinitions` and applies it AFTER
    /// `filterDeniedAgents`. The value is the one `REPL.tsx:3206-3208` merged in
    /// for the turn — this consumer never re-derives it from the agent files.
    #[test]
    fn agent_listing_delta_applies_selected_agent_type_restrictions() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _gate = EnvVarGuard::set("CLAUDE_CODE_AGENT_LIST_IN_MESSAGES", "true");
        let mut selected = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "reviewer",
            "Reviews changes",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::ProjectSettings,
        );
        selected.tools = Some(vec!["Agent(Explore)".to_string()]);
        let explore = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "Explore",
            "Explores code",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        let plan = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "Plan",
            "Plans work",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        let definitions = crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
            active_agents: vec![explore.clone(), plan.clone()],
            all_agents: vec![selected.clone(), explore.clone(), plan.clone()],
            failed_files: Vec::new(),
            allowed_agent_types: Some(vec!["Explore".to_string()]),
        };
        let mut context = ToolUseContext::default()
            .with_tools(vec![crate::tools::agent_tool::agent_tool_schema()])
            .with_agent_definitions(std::sync::Arc::new(definitions));
        context.agent_type = Some(selected.agent_type.clone());

        let attachment =
            get_agent_listing_delta_attachment(&context, &[]).expect("agent listing delta");
        assert_eq!(
            attachment.attachment.to_wire()["addedTypes"],
            serde_json::json!(["Explore"])
        );

        // Absent field → CC's `if (allowedAgentTypes)` is false and the listing
        // stays unrestricted, even though the selected agent's own frontmatter
        // still says `Agent(Explore)`: only the per-turn carrier decides.
        let unrestricted = crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
            active_agents: vec![explore.clone(), plan.clone()],
            all_agents: vec![selected.clone(), explore, plan],
            failed_files: Vec::new(),
            allowed_agent_types: None,
        };
        let mut context = ToolUseContext::default()
            .with_tools(vec![crate::tools::agent_tool::agent_tool_schema()])
            .with_agent_definitions(std::sync::Arc::new(unrestricted));
        context.agent_type = Some(selected.agent_type);
        let attachment =
            get_agent_listing_delta_attachment(&context, &[]).expect("agent listing delta");
        assert_eq!(
            attachment.attachment.to_wire()["addedTypes"],
            serde_json::json!(["Explore", "Plan"])
        );
    }

    #[test]
    fn mcp_instruction_delta_uses_live_initialize_instructions() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _instructions_lock = crate::services::mcp::client::TEST_MCP_INSTRUCTIONS_LOCK
            .lock()
            .unwrap();
        let _gate = EnvVarGuard::set("CLAUDE_CODE_MCP_INSTR_DELTA", "true");
        let _instructions =
            McpInstructionGuard::set("docs", "Use resource templates before tools.");
        let state = crate::state::app_state_store::McpState {
            clients: vec![crate::services::mcp::types::McpServerSnapshot {
                connection_id: None,
                client: crate::services::mcp::types::McpClientSnapshot {
                    name: "docs".to_string(),
                    status: crate::services::mcp::types::McpServerConnectionType::Connected,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: None,
                    error: None,
                },
                config: None,
                supports_resources: false,
                tools: Vec::new(),
                prompts: Vec::new(),
                resources: Vec::new(),
            }],
            ..Default::default()
        };
        let mut context = ToolUseContext::default().with_mcp_state(state);

        let attachment =
            get_mcp_instructions_delta_attachment(&context, &[]).expect("instruction delta");
        assert_eq!(attachment.attachment_type(), "mcp_instructions_delta");
        assert_eq!(
            attachment.attachment.to_wire()["addedBlocks"][0].as_str(),
            Some("## docs\nUse resource templates before tools.")
        );
    }

    #[test]
    fn mcp_instruction_delta_appends_chrome_tool_search_instructions() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _instructions_lock = crate::services::mcp::client::TEST_MCP_INSTRUCTIONS_LOCK
            .lock()
            .unwrap();
        let _gate = EnvVarGuard::set("CLAUDE_CODE_MCP_INSTR_DELTA", "true");
        let _tool_search = EnvVarGuard::set("ENABLE_TOOL_SEARCH", "true");
        let _instructions = McpInstructionGuard::set("claude-in-chrome", "Server guidance.");
        let state = crate::state::app_state_store::McpState {
            clients: vec![crate::services::mcp::types::McpServerSnapshot {
                connection_id: None,
                client: crate::services::mcp::types::McpClientSnapshot {
                    name: "claude-in-chrome".to_string(),
                    status: crate::services::mcp::types::McpServerConnectionType::Connected,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: None,
                    error: None,
                },
                config: None,
                supports_resources: false,
                tools: Vec::new(),
                prompts: Vec::new(),
                resources: Vec::new(),
            }],
            ..Default::default()
        };
        let mut context = ToolUseContext::default()
            .with_mcp_state(state)
            .with_main_loop_model("claude-sonnet-4-6")
            .with_tools(vec![
                crate::tools::tool_search_tool::tool_search_tool_schema(),
            ]);

        let attachment =
            get_mcp_instructions_delta_attachment(&context, &[]).expect("chrome delta");
        let wire = attachment.attachment.to_wire();
        let block = wire["addedBlocks"][0].as_str().expect("instruction block");
        assert!(block.starts_with("## claude-in-chrome\nServer guidance."));
        assert!(block.contains("Before using any chrome browser tools"));
        assert!(block.contains("select:mcp__claude-in-chrome__tabs_context_mcp"));
    }

    #[tokio::test]
    async fn lsp_diagnostic_attachments_skip_agent_source_without_draining_registry() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();
        register_pending_lsp_diagnostic("rust", vec![file("/tmp/a.rs", vec![diag("broken")])]);

        let mut context = ToolUseContext {
            tools: vec![crate::tools::bash_tool::bash_tool_schema()],
            ..ToolUseContext::default()
        };
        let messages = Vec::new();
        let attachments = get_attachments(GetAttachmentMessagesParams {
            tool_use_context: &mut context,
            messages: &messages,
            query_source: &QuerySource::Agent,
            queued_commands: Vec::new(),
        })
        .await;

        assert!(attachments.is_empty());
        assert_eq!(get_pending_lsp_diagnostic_count(), 1);
        reset_all_lsp_diagnostic_state();
    }
}
