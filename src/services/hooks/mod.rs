//! Shell command hooks subsystem.
//! Maps to: CC `utils/hooks.ts` — decomposed into focused service modules.
//!
//! CC's hook system executes user-configured shell commands at lifecycle
//! points: pre/post tool use, notifications, session start/end, status line,
//! file suggestions, compaction, worktree ops, etc. All hooks share a common
//! execution pipeline (shell spawn + JSON stdin + stdout parsing + trust checks).
//!
//! Naming distinction: `services::hooks` = shell command hooks (this module).
//! `crate::hooks` = iocraft component hooks (use_state, use_terminal_events).

pub mod exec;
pub mod instructions_loaded;
pub mod matching;
pub mod parsing;
pub mod security;
pub mod statusline;

pub mod compaction;
pub mod elicitation;
pub mod env;
pub mod file_suggestion;
pub mod lifecycle;
pub mod permission_request;
pub mod pre_tool;
pub mod prompt;
pub mod task;
pub mod teammate;
pub mod tool;
pub mod worktree;

use crate::utils::hooks::{hooks_config_snapshot, session_hooks};
use serde::{Deserialize, Serialize};

// ════════════════════════════════════════════════════════════
// HookEvent — maps to CC entrypoints/sdk/coreTypes.ts HOOK_EVENTS
// ════════════════════════════════════════════════════════════

/// Lifecycle event types for hook dispatch.
/// Maps to: CC `HookEvent`, SDK codegen re-exported through
/// `entrypoints/sdk/coreTypes.generated.ts` (generated-stub) →
/// `entrypoints/sdk/coreTypes.ts` → `entrypoints/agentSdkTypes.ts:24`. It is
/// NOT declared in `types/hooks.ts` (:4-9 imports it) nor in `utils/hooks.ts`
/// (:77 imports it), so the #162 types/hooks.ts extraction leaves it be;
/// housed here until an `entrypoints/sdk/core_types.rs` home exists (booked).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HookEvent {
    SessionStart,
    Setup,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionRequest,
    PermissionDenied,
    Notification,
    Elicitation,
    ElicitationResult,
    CwdChanged,
    FileChanged,
    WorktreeCreate,
    WorktreeRemove,
    SubagentStart,
    SubagentStop,
    Stop,
    StopFailure,
    PreCompact,
    PostCompact,
    TaskCreated,
    TaskCompleted,
    TeammateIdle,
    InstructionsLoaded,
    ConfigChange,
}

/// Maps to: CC `entrypoints/sdk/coreTypes.ts:25-53#HOOK_EVENTS`.
/// Preserve source order: schemas expose it in enum validation diagnostics and
/// registration/metadata consumers iterate this same canonical constant.
pub const HOOK_EVENTS: &[HookEvent] = &[
    HookEvent::PreToolUse,
    HookEvent::PostToolUse,
    HookEvent::PostToolUseFailure,
    HookEvent::Notification,
    HookEvent::UserPromptSubmit,
    HookEvent::SessionStart,
    HookEvent::SessionEnd,
    HookEvent::Stop,
    HookEvent::StopFailure,
    HookEvent::SubagentStart,
    HookEvent::SubagentStop,
    HookEvent::PreCompact,
    HookEvent::PostCompact,
    HookEvent::PermissionRequest,
    HookEvent::PermissionDenied,
    HookEvent::Setup,
    HookEvent::TeammateIdle,
    HookEvent::TaskCreated,
    HookEvent::TaskCompleted,
    HookEvent::Elicitation,
    HookEvent::ElicitationResult,
    HookEvent::ConfigChange,
    HookEvent::WorktreeCreate,
    HookEvent::WorktreeRemove,
    HookEvent::InstructionsLoaded,
    HookEvent::CwdChanged,
    HookEvent::FileChanged,
];

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::Setup => "Setup",
            Self::SessionEnd => "SessionEnd",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::PermissionRequest => "PermissionRequest",
            Self::PermissionDenied => "PermissionDenied",
            Self::Notification => "Notification",
            Self::Elicitation => "Elicitation",
            Self::ElicitationResult => "ElicitationResult",
            Self::CwdChanged => "CwdChanged",
            Self::FileChanged => "FileChanged",
            Self::WorktreeCreate => "WorktreeCreate",
            Self::WorktreeRemove => "WorktreeRemove",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::Stop => "Stop",
            Self::TaskCreated => "TaskCreated",
            Self::TaskCompleted => "TaskCompleted",
            Self::TeammateIdle => "TeammateIdle",
            Self::InstructionsLoaded => "InstructionsLoaded",
            Self::ConfigChange => "ConfigChange",
            Self::StopFailure => "StopFailure",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
        }
    }
}

// ════════════════════════════════════════════════════════════
// Hook config types — owned by schemas/hooks.rs (CC schemas/hooks.ts)
// ════════════════════════════════════════════════════════════

// The runtime re-exports the schema-owned types, mirroring how CC's hook
// execution reaches HookCommand/HookMatcher through re-exports rather than
// importing schemas/hooks directly (utils/hooks.ts imports them from
// utils/settings/types.js).
pub use crate::schemas::hooks::{
    HookCallback, HookCommand, HookConfigEntry, HooksConfig, RegisteredHook, RegisteredHookMatcher,
    RegisteredHooks,
};

/// Assemble settings-backed and registered plugin hooks without mutating the
/// settings snapshot.
///
/// Maps to: CC `utils/hooks.ts#getHooksConfig` for the two process-level hook
/// sources. Session-derived hooks remain scoped and merged by their callers.
pub fn load_hooks_config() -> hooks_config_snapshot::LoadedHooksConfig {
    let mut loaded = hooks_config_snapshot::load_hooks_config_from_settings_sources();
    if !loaded.disable_all_hooks {
        if let Some(registered) = crate::bootstrap::state::get_registered_hooks() {
            for (event, entries) in registered {
                loaded
                    .config
                    .entry(event)
                    .or_default()
                    .extend(entries.into_iter().filter(|entry| {
                        !loaded.allow_managed_hooks_only || entry.plugin_root.is_none()
                    }));
            }
        }
    }
    loaded
}

/// Assemble the full hook table CC's `executeHooks` runs against: settings +
/// registered (SDK/plugin) + the SESSION-derived hooks for `session_id`.
///
/// Maps to: CC `utils/hooks.ts:1492-1566#getHooksConfig`. `load_hooks_config()`
/// owns only the first two sources ("Session-derived hooks remain scoped and
/// merged by their callers", its own doc); this adds CC's third arm
/// (`:1541-1563`) under the same `shouldAllowManagedHooksOnly()` gate
/// (`:1516`, `:1534-1541` — "Skip session hooks entirely when
/// allowManagedHooksOnly is set").
///
/// The caller picks `session_id`, because CC does: `executeHooks` uses
/// `toolUseContext?.agentId ?? getSessionId()` (`:2003`) while
/// `executeHooksOutsideREPL` hardcodes `getSessionId()` (`:3040`, "Use main
/// session ID for outside-REPL hooks"). The two are not interchangeable —
/// `hooks.ts:3599-3602` spells out that a StopFailure gate keyed on `agentId`
/// would pass the gate and then fail execution.
pub fn load_hooks_config_with_session_hooks(session_id: &str) -> RegisteredHooks {
    let loaded = load_hooks_config();
    let mut config = loaded.config;
    if !loaded.allow_managed_hooks_only {
        session_hooks::merge_session_hooks_into_config(&mut config, session_id);
    }
    config
}

/// Maps to: CC `utils/hooks/hooksConfigSnapshot.ts:83-88`
/// `shouldDisableAllHooksIncludingManaged()` as read from its ZERO-argument
/// call sites — `utils/hooks.ts:1978` (`executeHooks`), `:3022`
/// (`executeHooksOutsideREPL`), `:4591` and `:4681`. CC reaches
/// `getSettingsForSource('policySettings')` inside the helper, so an executor
/// with no settings snapshot in hand still gets the managed gate; this wrapper
/// keeps that property for the Rust executors, which take a pre-resolved
/// `RegisteredHooks` table instead of loading settings themselves.
pub fn should_disable_all_hooks_including_managed_from_settings() -> bool {
    let policy_settings = crate::utils::settings::get_settings_for_source(
        crate::utils::settings::SettingSource::Policy,
    );
    hooks_config_snapshot::should_disable_all_hooks_including_managed(policy_settings.as_ref())
}

/// Maps to: CC `utils/hooks.ts:1987` / `:3021` —
/// ``const hookName = matchQuery ? `${hookEvent}:${matchQuery}` : hookEvent``.
///
/// JS truthiness: an empty `matchQuery` is falsy, so it yields the bare event
/// name rather than a trailing-colon string.
fn hook_name(event: HookEvent, match_query: &str) -> String {
    if match_query.is_empty() {
        event.as_str().to_string()
    } else {
        format!("{}:{match_query}", event.as_str())
    }
}

/// Maps to: CC `utils/hooks.ts:1982-1984` (`executeHooks`) and `:3016-3018`
/// (`executeHooksOutsideREPL`) — `if (isEnvTruthy(process.env.CLAUDE_CODE_SIMPLE))
/// return`, the `--bare` arm. `main.tsx:1359` describes the flag as "Minimal
/// mode: skip hooks, LSP, plugin sync, …", and `cli/dispatch.rs:9` is this
/// port's `--bare` writer.
///
pub fn bare_mode_disables_hooks() -> bool {
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_SIMPLE").as_deref(),
    )
}

/// The gate block CC opens BOTH of its hook executors with, before either one
/// resolves a session id or matches a single hook.
///
/// Maps to: CC `utils/hooks.ts:1978-1999` (`executeHooks`) and `:3016-3036`
/// (`executeHooksOutsideREPL`). All three entry gates apply here:
/// - `CLAUDE_CODE_SIMPLE` disables hook execution in bare mode;
///
/// - `shouldDisableAllHooksIncludingManaged()` (`:1978`, `:3022`) — the MANAGED
///   `disableAllHooks` policy, i.e. an admin control that user settings cannot
///   re-enable;
/// - `shouldSkipHookDueToTrust()` (`:1994`, `:3031`) — "SECURITY: ALL hooks
///   require workspace trust in interactive mode. This centralized check
///   prevents RCE vulnerabilities for all current and future hooks".
///
/// Both are PER-CALL, not per-hook. CC evaluates them once at executor entry,
/// ahead of `getMatchingHooks` (`:2004`, `:3041`) and therefore ahead of the
/// loop over matched hooks, so a config that mixes entries is not filtered
/// entry-by-entry — the whole call returns nothing. `executeStopHooks` spells
/// the intent out at `:3687`: "Trust check is now centralized in executeHooks()".
///
/// The trust arm's DEFAULT is skip, and that is CC's:
/// `computeTrustDialogAccepted()` (`utils/config.ts:705-743`) returns `false`
/// when neither session trust nor any ancestor of the project path / cwd carries
/// `hasTrustDialogAccepted`, and `shouldSkipHookDueToTrust()` (`hooks.ts:286-296`)
/// turns that into `true` for every interactive session. A workspace whose trust
/// was never accepted runs NO hooks in CC either. This port's
/// `check_has_trust_dialog_accepted()` is if anything more permissive than CC's
/// (it parent-walks BOTH the project path and the cwd, where CC exact-matches
/// the project path and walks only the cwd), so it cannot skip where CC runs.
///
/// This is a SHARED ENTRY rather than a copy per executor on purpose. CC has
/// exactly two functions that reach `execCommandHook`, and every hook event
/// funnels through one of them, so CC gets the whole system from one gate. The
/// user-authorized `services/hooks/` decomposition split those two into a dozen
/// event executors; gating them one at a time is precisely what left four
/// covered and the other ten running user commands under an enterprise
/// `disableAllHooks`.
///
/// Deviation (diagnostics only): CC's `executeHooks` cannot log its
/// `disableAllHooks` skip, because it builds `hookName` only AFTER the early
/// returns (`:1986-1987`), while `executeHooksOutsideREPL` builds it first
/// (`:3021`) and does log. This entry builds the name up front and logs in both
/// families — a strict superset of CC's `--debug` output, with identical
/// control flow.
pub fn should_skip_hook_execution(event: HookEvent, match_query: &str) -> bool {
    // CC executeHooks checks managed policy, simple mode, then trust.
    if should_disable_all_hooks_including_managed_from_settings() {
        crate::utils::debug::log_for_debugging(&format!(
            "Skipping hooks for {} due to 'disableAllHooks' managed setting",
            hook_name(event, match_query)
        ));
        return true;
    }
    if bare_mode_disables_hooks() {
        return true;
    }
    if security::should_skip_hook_due_to_trust(
        crate::utils::config::check_has_trust_dialog_accepted(),
        crate::bootstrap::state::get_is_non_interactive_session(),
    ) {
        crate::utils::debug::log_for_debugging(&format!(
            "Skipping {} hook execution - workspace trust not accepted",
            hook_name(event, match_query)
        ));
        return true;
    }
    false
}

/// Run a hook future from synchronous startup/component seams.
///
/// Hook commands use `tokio::process`; startup resume can run before the main
/// render runtime is entered, while REPL-local callers already have a Tokio
/// handle. Supply a small current-thread runtime only for the former case.
pub fn block_on_hook_future<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        futures::executor::block_on(future)
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create hook runtime")
            .block_on(future)
    }
}

// ════════════════════════════════════════════════════════════
// HookResult — maps to CC utils/hooks.ts:338-357 (the LOCAL interface)
// ════════════════════════════════════════════════════════════

/// Outcome of a single hook execution.
/// Maps to: CC `utils/hooks.ts:342` `HookResult.outcome` — the
/// `'success' | 'blocking' | 'non_blocking_error' | 'cancelled'` union, lifted
/// into a named enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    Success,
    Blocking,
    NonBlockingError,
    Cancelled,
}

/// Maps to: CC `utils/hooks.ts:1882-1887#getPreToolHookBlockingMessage`.
pub fn get_pre_tool_hook_blocking_message(
    hook_name: &str,
    blocking_error: &HookBlockingError,
) -> String {
    format!("{hook_name} hook error: {}", blocking_error.blocking_error)
}

/// Elicitation response returned by hook-specific output.
/// Maps to: CC `utils/hooks.ts:335-336` `ElicitationResponse` — a re-export of
/// the MCP SDK `ElicitResult` ("for backward compat"), projected here as the
/// two fields hook output actually carries (action + content).
#[derive(Debug, Clone, PartialEq)]
pub struct HookElicitationResponse {
    pub action: String,
    pub content: Option<serde_json::Value>,
}

/// Result of executing a single hook.
/// Maps to: CC `utils/hooks.ts:338-357` `HookResult` — the interface
/// `utils/hooks.ts` declares LOCALLY (it does not import the SDK-facing
/// `types/hooks.ts:260-275` `HookResult`, whose `message`/`systemMessage` are
/// `Message` and which lacks the elicitation/watchPaths/retry fields carried
/// here). Port-side extras beyond CC's shape: `command`, `duration_ms`,
/// `stdout`, `stderr` (summary rendering); CC's `hook` back-reference (:356)
/// is carried by H: the executor's concrete hook type. This is a Rust
/// projection of the source union; existing command/callback callers retain
/// RegisteredHook while the standalone Agent executor retains AgentHook.
#[derive(Debug, Clone)]
pub struct HookResult<H = RegisteredHook> {
    /// Maps to CC HookResult.hook and HookResult.message.
    pub hook: Option<H>,
    pub message: Option<crate::types::message::AttachmentMessage>,
    pub outcome: HookOutcome,
    /// User-facing system message from hook output.
    pub system_message: Option<String>,
    /// Blocking error that prevents tool execution.
    pub blocking_error: Option<HookBlockingError>,
    /// Whether the query loop should stop after this hook.
    pub prevent_continuation: bool,
    /// Reason for stopping (from hook JSON output).
    pub stop_reason: Option<String>,
    /// Permission behavior override from PreToolUse hooks.
    pub permission_behavior: Option<PermissionBehavior>,
    /// Reason for the permission decision.
    pub hook_permission_decision_reason: Option<String>,
    /// CC hooks.ts:2865: source of the currently completing hook, even when
    /// the aggregated permission behavior was established by an earlier hook.
    pub hook_source: Option<String>,
    /// Additional context to inject into system prompt.
    pub additional_context: Option<String>,
    /// Initial user message for SessionStart hooks.
    pub initial_user_message: Option<String>,
    /// Updated tool input from PreToolUse hooks.
    pub updated_input: Option<serde_json::Value>,
    /// Updated MCP tool output from PostToolUse hooks.
    pub updated_mcp_tool_output: Option<serde_json::Value>,
    /// Whether the hook requests a retry of the current operation.
    pub retry: Option<bool>,
    /// File paths to watch for changes (SessionStart hooks).
    pub watch_paths: Option<Vec<String>>,
    /// Permission request result from PermissionRequest hooks.
    /// Maps to: CC `utils/hooks.ts:351` `HookResult.permissionRequestResult`;
    /// the union type itself is `types/hooks.ts:248-259` material and lives in
    /// `crate::types::hooks` (#162), reached over exactly as
    /// `utils/hooks.ts:66-75` imports it.
    pub permission_request_result: Option<crate::types::hooks::PermissionRequestResult>,
    /// Elicitation hook response. Maps to CC `HookResult.elicitationResponse`.
    pub elicitation_response: Option<HookElicitationResponse>,
    /// ElicitationResult hook response. Maps to CC
    /// `HookResult.elicitationResultResponse`.
    pub elicitation_result_response: Option<HookElicitationResponse>,
    /// Updated permission rules from PermissionRequest hook allow decisions.
    /// Maps to: CC `types/hooks.ts:252` `PermissionRequestResult`
    /// `updatedPermissions` (allow arm), flattened here as a port-side
    /// projection — see the enum's doc in `crate::types::hooks`.
    pub permission_updates: Vec<crate::types::permissions::PermissionUpdate>,
    /// Hook command used for official Stop/PreToolUse summary messages.
    pub command: Option<String>,
    /// Wall-clock hook duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Raw stdout, used to decide whether a summary has hidden output.
    pub stdout: Option<String>,
    /// Raw stderr, used for non-blocking error summaries.
    pub stderr: Option<String>,
}

impl<H> Default for HookResult<H> {
    fn default() -> Self {
        Self {
            outcome: HookOutcome::Success,
            hook: None,
            message: None,
            system_message: None,
            blocking_error: None,
            prevent_continuation: false,
            stop_reason: None,
            permission_behavior: None,
            hook_permission_decision_reason: None,
            hook_source: None,
            additional_context: None,
            initial_user_message: None,
            updated_input: None,
            updated_mcp_tool_output: None,
            retry: None,
            watch_paths: None,
            permission_request_result: None,
            elicitation_response: None,
            elicitation_result_response: None,
            permission_updates: Vec::new(),
            command: None,
            duration_ms: None,
            stdout: None,
            stderr: None,
        }
    }
}

/// Blocking error from a hook.
/// Maps to: CC `utils/hooks.ts:330-333` `HookBlockingError { blockingError:
/// string; command: string }`. Serde mirrors the wire object CC embeds as the
/// `hook_blocking_error` attachment's `blockingError` field
/// (utils/attachments.ts:354-360, services/tools/toolHooks.ts:105-115).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookBlockingError {
    pub blocking_error: String,
    pub command: String,
}

/// Maps to CC `utils/hooks.ts` `getStopHookMessage(...)`.
///
/// Misplaced: this is Stop-event-specific with a single consumer
/// (`query/stop_hooks.rs`), so it belongs beside `execute_stop_hooks` in
/// `lifecycle.rs`, not in this shared root. Left here to keep the move out of
/// an unrelated batch; its UserPromptSubmit sibling already sits in
/// `prompt.rs`.
pub fn get_stop_hook_message(blocking_error: &HookBlockingError) -> String {
    format!("Stop hook feedback:\n{}", blocking_error.blocking_error)
}

/// Permission behavior from PreToolUse hooks.
/// Maps to: CC `utils/hooks.ts:345` `HookResult.permissionBehavior` — the
/// `'ask' | 'deny' | 'allow' | 'passthrough'` inline union, lifted into a
/// named enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
    Passthrough,
}

// ════════════════════════════════════════════════════════════
// CommandExecResult — low-level shell execution output
// ════════════════════════════════════════════════════════════

/// Raw result of spawning and waiting for a shell command.
/// Maps to: CC `execCommandHook` return value.
#[derive(Debug, Clone)]
pub struct CommandExecResult {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
    pub aborted: bool,
}

// ════════════════════════════════════════════════════════════
// createBaseHookInput — maps to CC utils/hooks.ts:301-328
// ════════════════════════════════════════════════════════════

/// Runtime context passed to hook JSON input payloads.
/// Callers build this once per session and pass it into hook execution.
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: String,
    pub permission_mode: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
}

/// Build the base JSON fields shared by all hook input payloads.
/// Maps to: CC `createBaseHookInput()` (hooks.ts:301-328) and its schema twin
/// `BaseHookInputSchema` (`entrypoints/sdk/coreSchemas.ts:387-411`).
///
/// Every hook type spreads these fields into its event-specific payload — all
/// 27 of them, verified by `rg -n '\.\.\.createBaseHookInput\('` over
/// `../rebuild/src/` (28 hits: 25 in `utils/hooks.ts`, plus StatusLine and
/// fileSuggestions, which are not `HookInput` events).
///
/// Key set is exactly six:
/// - `session_id`, `transcript_path`, `cwd` — always present;
/// - `permission_mode`, `agent_id`, `agent_type` — `z.string().optional()`, and
///   `JSON.stringify` drops a key whose value is `undefined`, so an absent one
///   sends NO key rather than `null`. The port therefore inserts an optional
///   only when it is `Some`.
///
/// `project_dir` and `claude_code_version` were port-side additions and are
/// removed: CC's only `project_dir` is nested under the StatusLine payload's
/// `workspace` object (`components/StatusLine.tsx:118-122`), and
/// `claude_code_version` belongs to the SDK `system.init` message
/// (`utils/messages/systemInit.ts:74`, `coreSchemas.ts:1464`) — neither is a
/// hook input field. CC puts the project dir on the env rail instead, as
/// `CLAUDE_PROJECT_DIR`, which `exec::exec_command_hook` sets for every hook
/// (`utils/hooks.ts:816,882-885`).
///
/// Deviation (carrier): CC's `sessionId ?? getSessionId()` (`:315`),
/// `getTranscriptPathForSession(resolvedSessionId)` (`:322`) and `getCwd()`
/// (`:323`) are nullish fallbacks over optional arguments. `HookContext`
/// carries those three as plain `String`, so "absent" is the empty string and
/// the fallback fires on empty rather than on `None`.
pub fn create_base_hook_input(ctx: &HookContext) -> serde_json::Value {
    serde_json::Value::Object(create_base_hook_input_object(ctx))
}

/// Assembles one hook input: the `createBaseHookInput` spread followed by the
/// event-specific keys, in CC's object-literal order.
///
/// [`Self::set_optional`] is the port's stand-in for `JSON.stringify` dropping
/// an object property whose value is `undefined` — an absent optional sends NO
/// key, not `null`. Hook scripts branch on key presence (`'agent_id' in data`),
/// so the two are not interchangeable.
pub struct HookInputBuilder(serde_json::Map<String, serde_json::Value>);

impl HookInputBuilder {
    /// Start from a `createBaseHookInput(...)` spread.
    pub fn from_base(base: serde_json::Map<String, serde_json::Value>) -> Self {
        Self(base)
    }

    pub fn set(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.0.insert(key.to_string(), value.into());
        self
    }

    pub fn set_optional(mut self, key: &str, value: Option<impl Into<serde_json::Value>>) -> Self {
        if let Some(value) = value {
            self.0.insert(key.to_string(), value.into());
        }
        self
    }

    pub fn build(self) -> serde_json::Value {
        serde_json::Value::Object(self.0)
    }
}

/// Map-returning form of [`create_base_hook_input`], for the builders that
/// spread the base and then insert their event-specific keys.
pub fn create_base_hook_input_object(
    ctx: &HookContext,
) -> serde_json::Map<String, serde_json::Value> {
    let session_id = if ctx.session_id.is_empty() {
        crate::bootstrap::state::get_session_id()
    } else {
        ctx.session_id.clone()
    };
    let transcript_path = if ctx.transcript_path.is_empty() {
        // CC keys the transcript on the SESSION, not on the agent: a subagent's
        // own transcript reaches hooks through SubagentStop's
        // `agent_transcript_path` (`hooks.ts:3676`), never through the base.
        // hooks.ts:324 resolves the explicit target before choosing its path.
        crate::utils::session_storage::get_transcript_path_for_session(&session_id)
            .display()
            .to_string()
    } else {
        ctx.transcript_path.clone()
    };
    let cwd = if ctx.cwd.is_empty() {
        std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string()
    } else {
        ctx.cwd.clone()
    };

    let mut object = serde_json::Map::new();
    object.insert("session_id".to_string(), serde_json::json!(session_id));
    object.insert(
        "transcript_path".to_string(),
        serde_json::json!(transcript_path),
    );
    object.insert("cwd".to_string(), serde_json::json!(cwd));
    if let Some(permission_mode) = ctx.permission_mode.as_ref() {
        object.insert(
            "permission_mode".to_string(),
            serde_json::json!(permission_mode),
        );
    }
    if let Some(agent_id) = ctx.agent_id.as_ref() {
        object.insert("agent_id".to_string(), serde_json::json!(agent_id));
    }
    // CC also falls back to `getMainThreadAgentType()` here (`:319`); the port
    // has no process-level main-thread agent type (it lives on the REPL's
    // `main_thread_agent_definition`), so only an explicit value is sent.
    if let Some(agent_type) = ctx.agent_type.as_ref() {
        object.insert("agent_type".to_string(), serde_json::json!(agent_type));
    }
    object
}

/// Classify a hook execution result by exit code.
/// Maps to: CC hooks.ts:2196-2220 exit-code-2 blocking semantics.
///
/// - Exit 0: success
/// - Exit 2: blocking (hook wants to prevent the operation)
/// - Other non-zero: non-blocking error
/// - Aborted (timeout): cancelled
pub fn classify_exit_code(result: &CommandExecResult) -> HookOutcome {
    if result.aborted {
        HookOutcome::Cancelled
    } else if result.status == 0 {
        HookOutcome::Success
    } else if result.status == 2 {
        HookOutcome::Blocking
    } else {
        HookOutcome::NonBlockingError
    }
}

/// Shared test scaffolding for the hook subsystem.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::utils::test_env::EnvVarGuard;

    /// Fold a settings-shaped `HooksConfig` fixture into the execution-facing
    /// `RegisteredHooks` table, the same `from_config_entry` merge the loading
    /// chain performs before hooks reach `get_matching_hooks`.
    pub(crate) fn registered_config(
        config: &super::HooksConfig,
    ) -> crate::schemas::hooks::RegisteredHooks {
        config
            .iter()
            .map(|(event, entries)| {
                (
                    event.clone(),
                    entries
                        .iter()
                        .map(crate::schemas::hooks::RegisteredHookMatcher::from_config_entry)
                        .collect(),
                )
            })
            .collect()
    }

    /// Records what a hook actually received on stdin.
    ///
    /// The hook command is `cat > "$COMETIX_TEST_HOOK_INPUT_CAPTURE"`, and the
    /// destination rides the callers' `base_env` channel into
    /// `exec_command_hook`, so the capture goes through the real spawn path
    /// rather than re-deriving the payload in the test.
    #[cfg(not(windows))]
    pub(crate) struct HookInputCapture {
        path: std::path::PathBuf,
    }

    #[cfg(not(windows))]
    impl HookInputCapture {
        pub(crate) fn new() -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "cometix-hook-input-capture-{}.json",
                    uuid::Uuid::new_v4().simple()
                )),
            }
        }

        pub(crate) fn config(
            &self,
            event: &str,
            matcher: Option<&str>,
        ) -> crate::schemas::hooks::RegisteredHooks {
            let config: super::HooksConfig = std::collections::HashMap::from([(
                event.to_string(),
                vec![super::HookConfigEntry {
                    matcher: matcher.map(str::to_string),
                    hooks: vec![super::HookCommand {
                        command: r#"cat > "$COMETIX_TEST_HOOK_INPUT_CAPTURE""#.to_string(),
                        shell: None,
                        timeout: Some(10),
                        condition: None,
                        status: None,
                        once: None,
                        is_async: None,
                        async_rewake: None,
                    }],
                    plugin_root: None,
                    plugin_name: None,
                    plugin_id: None,
                }],
            )]);
            registered_config(&config)
        }

        pub(crate) fn base_env(&self) -> Vec<(String, String)> {
            vec![(
                "COMETIX_TEST_HOOK_INPUT_CAPTURE".to_string(),
                self.path.display().to_string(),
            )]
        }

        pub(crate) fn read(&self) -> serde_json::Value {
            let raw = std::fs::read_to_string(&self.path)
                .unwrap_or_else(|error| panic!("the hook received no stdin: {error}"));
            serde_json::from_str(&raw).expect("the hook input is JSON")
        }
    }

    #[cfg(not(windows))]
    impl Drop for HookInputCapture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Installs a managed (policy) settings file for the lifetime of the guard.
    ///
    /// `CLAUDE_CONFIG_DIR` (pinned by `just test`) does NOT cover the managed
    /// settings root, so a hook test that means to assert on policy behaviour —
    /// or to assert that policy is ABSENT — has to redirect
    /// `CLAUDE_CODE_MANAGED_SETTINGS_PATH` itself. Requires `TEST_ENV_LOCK`.
    pub(crate) struct ManagedSettingsGuard {
        root: std::path::PathBuf,
        env: Option<EnvVarGuard>,
    }

    impl ManagedSettingsGuard {
        /// `contents = None` installs an EMPTY managed root, which is how a test
        /// states "no policy" without depending on the developer's machine.
        pub(crate) fn install(contents: Option<&str>) -> Self {
            let root = std::env::temp_dir().join(format!(
                "cometix-hooks-managed-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).expect("create managed settings root");
            if let Some(contents) = contents {
                std::fs::write(root.join("managed-settings.json"), contents)
                    .expect("write managed settings");
            }
            let env = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root);
            // A direct disk write bypasses production invalidation, so the test
            // states the invariant itself (same note as `settings/mod.rs` tests).
            crate::utils::settings::settings_cache::reset_settings_cache();
            Self {
                root,
                env: Some(env),
            }
        }
    }

    impl Drop for ManagedSettingsGuard {
        fn drop(&mut self) {
            drop(self.env.take());
            crate::utils::settings::settings_cache::reset_settings_cache();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Accepts workspace trust for the lifetime of the guard.
    ///
    /// Every hook executor now opens with
    /// [`super::should_skip_hook_execution`], whose trust arm returns an empty
    /// result set when `should_skip_hook_due_to_trust(...)` says the workspace
    /// is untrusted. A hook test that lands on the skip path does not assert on
    /// hook behaviour at all, and every "expected N results, got 0" then reads
    /// as a product bug instead of a missing fixture.
    ///
    /// Under `just test` the answer today is TRUSTED, not untrusted: `just
    /// _prep` rebuilds mutable `target/test-home`, serializes `.claude.json`
    /// with exactly the canonical tracked `tests/fixtures/isolated-project`
    /// trust key, and pins that project as the original cwd. So this guard is
    /// belt-and-braces against a test that relocates the config home — it is
    /// [`WorkspaceTrustDeniedGuard`] that has to do real work to reach the other
    /// branch.
    pub(crate) struct SessionTrustGuard(bool);

    impl SessionTrustGuard {
        pub(crate) fn accepted() -> Self {
            let previous = crate::bootstrap::state::get_session_trust_accepted();
            crate::bootstrap::state::set_session_trust_accepted(true);
            Self(previous)
        }
    }

    impl Drop for SessionTrustGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_session_trust_accepted(self.0);
            // `check_has_trust_dialog_accepted` LATCHES its answer in a
            // process-wide cache (`utils/config.rs:1784-1795`), so restoring the
            // session flag alone would leave a later untrusted assertion reading
            // this guard's `true`.
            crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
        }
    }

    /// Forces the untrusted branch of CC `shouldSkipHookDueToTrust()`
    /// (`utils/hooks.ts:286-296`) for the lifetime of the guard.
    ///
    /// Reaching it takes all four of these, because the port's trust answer is
    /// an OR over four inputs:
    /// - `set_is_interactive(true)` + the three `CLAUDE_CODE_NON_INTERACTIVE` /
    ///   `COMETIX_NON_INTERACTIVE*` overrides cleared, because CC returns
    ///   "do not skip" outright in non-interactive (SDK) mode (`:288-291`);
    /// - `set_session_trust_accepted(false)`, the memory-only home-dir accept;
    /// - the latched `TRUST_DIALOG_ACCEPTED_CACHE` reset;
    /// - `set_original_cwd(<scratch>)`, because the tracked
    ///   `tests/fixtures/isolated-project` IS trusted by the canonical JSON in
    ///   `target/test-home/.claude.json` — an untrusted test that skipped this
    ///   step would silently assert nothing.
    ///
    /// Requires `TEST_ENV_LOCK`.
    pub(crate) struct WorkspaceTrustDeniedGuard {
        previous_session_trust: bool,
        previous_cwd: std::path::PathBuf,
        previous_interactive: bool,
        scratch: std::path::PathBuf,
        _non_interactive_env: Vec<EnvVarGuard>,
    }

    impl WorkspaceTrustDeniedGuard {
        pub(crate) fn install() -> Self {
            let scratch = std::env::temp_dir().join(format!(
                "cometix-hooks-untrusted-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&scratch).expect("create untrusted scratch cwd");
            let guard = Self {
                previous_session_trust: crate::bootstrap::state::get_session_trust_accepted(),
                previous_cwd: crate::bootstrap::state::get_original_cwd(),
                previous_interactive: crate::bootstrap::state::get_is_interactive(),
                scratch: scratch.clone(),
                _non_interactive_env: [
                    "CLAUDE_CODE_NON_INTERACTIVE",
                    "COMETIX_NON_INTERACTIVE",
                    "COMETIX_NON_INTERACTIVE_SESSION",
                ]
                .into_iter()
                .map(EnvVarGuard::unset)
                .collect(),
            };
            crate::bootstrap::state::set_is_interactive(true);
            crate::bootstrap::state::set_session_trust_accepted(false);
            crate::bootstrap::state::set_original_cwd(scratch);
            crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
            assert!(
                !crate::utils::config::check_has_trust_dialog_accepted(),
                "the guard has to actually reach the untrusted branch"
            );
            crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
            guard
        }
    }

    impl Drop for WorkspaceTrustDeniedGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(self.previous_cwd.clone());
            crate::bootstrap::state::set_session_trust_accepted(self.previous_session_trust);
            crate::bootstrap::state::set_is_interactive(self.previous_interactive);
            self._non_interactive_env.clear();
            crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
            let _ = std::fs::remove_dir_all(&self.scratch);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    /// Every function that can reach `exec_command_hook`, grouped by the
    /// executor loop it goes through. The list is closed: `rg -l
    /// 'exec_command_hook\(|exec_callback_hook\(' src/` now returns TWELVE
    /// files, all inside this module — `compaction`, `elicitation`, `env`,
    /// `exec` (the spawner itself), `file_suggestion`, `lifecycle`, `prompt`,
    /// `statusline`, `task`, `teammate`, `tool` and `worktree`. Until this
    /// batch it also returned `query/stop_hooks.rs`, from OUTSIDE the module.
    ///
    /// That outsider was named by this doc comment from the day the list was
    /// written and still left out of it, which is how it stayed ungated through
    /// 8bb2e62: `query/stop_hooks.rs#execute_stop_hooks_with_progress_events`
    /// was a port-side copy of `lifecycle::execute_stop_hooks`' loop with
    /// progress events bolted on, and copies do not inherit gates. Being
    /// outside `services/hooks/` is not a reason to leave it out — CC has ONE
    /// `executeStopHooks` (`hooks.ts:3639-3697`) that ends in `yield*
    /// executeHooks({…})`, so in CC the Stop path is gated by construction.
    ///
    /// That copy is now deleted and the REPL's progress path runs
    /// `lifecycle::execute_event` with a progress sink, which is why the last
    /// entry names that loop too. The ROW survives the function: reaching the
    /// shared loop through `handle_stop_hooks_with_config` is the end-of-turn
    /// path a user hits every turn, and it is what read as gated-by-name and
    /// ran anyway. Two entries naming one loop is the point — they are two
    /// call paths into it.
    ///
    /// `pre_tool` and `permission_request` are absent because they delegate to
    /// `tool::execute_hooks`; `statusline`/`file_suggestion` are absent because
    /// CC gates those in `executeStatusLineCommand` (`hooks.ts:4591-4602`) and
    /// `executeFileSuggestionCommand` (`:4681-4692`), not in the two hook-event
    /// executors this entry maps to — `statusline.rs` carries both gates as
    /// caller-supplied parameters already.
    #[cfg(not(windows))]
    const EXECUTOR_FAMILIES: &[&str] = &[
        "tool::execute_hooks",
        "env::execute_hooks_outside_repl_with_config",
        "compaction::execute_pre_compact_hooks",
        "compaction::execute_post_compact_hooks",
        "lifecycle::execute_event",
        "lifecycle::execute_session_end_hooks",
        "task::execute_task_event",
        "prompt::execute_user_prompt_submit_hooks",
        "teammate::execute_teammate_idle_hooks",
        "teammate::execute_subagent_start_hooks",
        "teammate::execute_subagent_stop_hooks",
        "worktree::execute_worktree_create_hook",
        "worktree::execute_worktree_remove_hook",
        "elicitation::execute_elicitation_hooks",
        "elicitation::execute_elicitation_result_hooks",
        "lifecycle::execute_event (query::stop_hooks progress path)",
    ];

    /// A matcher-free `printf` hook on every event any executor family reads.
    #[cfg(not(windows))]
    fn hook_on_every_event() -> RegisteredHooks {
        let entry = serde_json::json!({
            "hooks": [{"command": "printf ran-hook", "timeout": 5}]
        });
        let config: HooksConfig = serde_json::from_value(serde_json::Value::Object(
            HOOK_EVENTS
                .iter()
                .map(|event| {
                    (
                        event.as_str().to_string(),
                        serde_json::Value::Array(vec![entry.clone()]),
                    )
                })
                .collect(),
        ))
        .expect("the all-events fixture parses");
        test_support::registered_config(&config)
    }

    /// Runs one call through every executor family and reports, per family,
    /// whether its hook actually ran.
    #[cfg(not(windows))]
    async fn run_every_executor_family() -> Vec<(&'static str, bool)> {
        let config = hook_on_every_event();
        let context = HookContext::default();
        let abort = crate::tool::AbortController::default();

        vec![
            (
                "tool::execute_hooks",
                !tool::execute_post_tool_hooks(
                    &config,
                    "Read",
                    "toolu_gate",
                    &serde_json::json!({"file_path": "/tmp/source.txt"}),
                    &serde_json::json!({"type": "text"}),
                    &context,
                    vec![],
                    None,
                )
                .await
                .is_empty(),
            ),
            (
                "env::execute_hooks_outside_repl_with_config",
                !env::execute_hooks_outside_repl_with_config(
                    &config,
                    serde_json::json!({
                        "hook_event_name": "CwdChanged",
                        "old_cwd": "/old",
                        "new_cwd": "/new",
                    }),
                    None,
                    5_000,
                    vec![],
                )
                .await
                .is_empty(),
            ),
            (
                "compaction::execute_pre_compact_hooks",
                compaction::execute_pre_compact_hooks(&config, "manual", None, &context, &abort)
                    .await
                    .user_display_message
                    .is_some(),
            ),
            (
                "compaction::execute_post_compact_hooks",
                compaction::execute_post_compact_hooks(
                    &config, "manual", "summary", &context, &abort,
                )
                .await
                .user_display_message
                .is_some(),
            ),
            (
                "lifecycle::execute_event",
                !lifecycle::execute_stop_hooks(
                    &config,
                    Some("default"),
                    false,
                    None,
                    vec![],
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .is_empty(),
            ),
            (
                "lifecycle::execute_session_end_hooks",
                !lifecycle::execute_session_end_hooks(&config, "clear", vec![])
                    .await
                    .is_empty(),
            ),
            (
                "task::execute_task_event",
                !task::execute_task_created_hooks(
                    &config,
                    "task-1",
                    "Review auth",
                    None,
                    None,
                    None,
                    vec![],
                )
                .await
                .is_empty(),
            ),
            (
                "prompt::execute_user_prompt_submit_hooks",
                !prompt::execute_user_prompt_submit_hooks(&config, "a prompt", "default", vec![])
                    .await
                    .is_empty(),
            ),
            (
                "teammate::execute_teammate_idle_hooks",
                !teammate::execute_teammate_idle_hooks(&config, "ada", "alpha", None, vec![])
                    .await
                    .is_empty(),
            ),
            (
                "teammate::execute_subagent_start_hooks",
                !teammate::execute_subagent_start_hooks(
                    &config,
                    "agent-1",
                    "general-purpose",
                    vec![],
                )
                .await
                .is_empty(),
            ),
            (
                "teammate::execute_subagent_stop_hooks",
                !teammate::execute_subagent_stop_hooks(
                    &config,
                    "agent-1",
                    "general-purpose",
                    "/tmp/agent-agent-1.jsonl",
                    None,
                    vec![],
                )
                .await
                .is_empty(),
            ),
            (
                "worktree::execute_worktree_create_hook",
                worktree::execute_worktree_create_hook(
                    &config,
                    "agent-1234",
                    serde_json::json!({}),
                    vec![],
                )
                .await
                .is_ok(),
            ),
            (
                "worktree::execute_worktree_remove_hook",
                worktree::execute_worktree_remove_hook(
                    &config,
                    "/tmp/worktree",
                    serde_json::json!({}),
                    vec![],
                )
                .await,
            ),
            (
                "elicitation::execute_elicitation_hooks",
                !elicitation::execute_elicitation_hooks(
                    &config,
                    &context,
                    "docs",
                    "Authorize",
                    None,
                    None,
                    None,
                    None,
                    vec![],
                )
                .await
                .is_empty(),
            ),
            (
                "elicitation::execute_elicitation_result_hooks",
                !elicitation::execute_elicitation_result_hooks(
                    &config,
                    &context,
                    "docs",
                    "accept",
                    None,
                    None,
                    None,
                    vec![],
                )
                .await
                .is_empty(),
            ),
            (
                "lifecycle::execute_event (query::stop_hooks progress path)",
                {
                    // An `event_tx` is what supplies the progress sink, and it
                    // is the branch the REPL always takes. The receiver has to
                    // stay alive: the executor returns an empty result set when
                    // a progress send fails, which would make an UNGATED family
                    // read as gated.
                    let (event_tx, _event_rx) = async_channel::unbounded();
                    !crate::query::stop_hooks::handle_stop_hooks_with_config(
                        crate::query::stop_hooks::StopHookParams {
                            messages_for_query: Vec::new(),
                            assistant_messages: Vec::new(),
                            system_prompt: Vec::new(),
                            user_context: std::collections::BTreeMap::new(),
                            system_context: std::collections::BTreeMap::new(),
                            tool_use_context: crate::tool::ToolUseContext::default(),
                            query_source: crate::constants::query_source::QuerySource::Prompt,
                            stop_hook_active: None,
                            event_tx: Some(event_tx.into()),
                        },
                        &config,
                        vec![],
                    )
                    .await
                    .messages
                    .is_empty()
                },
            ),
        ]
    }

    #[cfg(not(windows))]
    fn families_that_ran(outcome: &[(&'static str, bool)]) -> Vec<&'static str> {
        outcome
            .iter()
            .filter(|(_, ran)| *ran)
            .map(|(family, _)| *family)
            .collect()
    }

    /// Maps to: CC `utils/hooks.ts:1978-1980` (`executeHooks`) and `:3022-3027`
    /// (`executeHooksOutsideREPL`) — `shouldDisableAllHooksIncludingManaged()`
    /// is the FIRST thing both executors do, so every hook event in CC is
    /// stopped by one gate.
    ///
    /// The claim under test is structural, not per-event: with the guard behind
    /// [`should_skip_hook_execution`], every family listed in
    /// [`EXECUTOR_FAMILIES`] stops, and so would a seventeenth added tomorrow.
    ///
    /// Non-vacuity is asserted in-test rather than by deleting the guard: the
    /// SAME sixteen calls run first against an EMPTY managed root (all sixteen
    /// must run) and then with the policy installed (none may). Neutering the
    /// guard turns the second half into a sixteen-name diff, and breaking the
    /// fixture turns the first half into one.
    ///
    /// Old shape (3933d3e, before 8bb2e62): only `tool::execute_hooks`,
    /// `env::execute_hooks_outside_repl_with_config` and the two compaction
    /// entries were gated. The second half FAILED with the other eleven
    /// families listed as still running — a hard assertion failure, not a hang.
    ///
    /// Old shape (8bb2e62, before this batch): the sixteenth family,
    /// `query::stop_hooks::execute_stop_hooks_with_progress_events`, was absent
    /// from the list AND ungated, so an enterprise `disableAllHooks` still ran
    /// Stop hooks down the REPL's progress path. Adding the entry first, before
    /// the gate, produced exactly the one-name diff described above.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn managed_disable_all_hooks_policy_stops_every_executor_family() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _trust = test_support::SessionTrustGuard::accepted();

        // Baseline: an EMPTY managed root, so the developer's real policy file
        // cannot make this half pass for the wrong reason.
        let allowed = {
            let _managed = test_support::ManagedSettingsGuard::install(None);
            run_every_executor_family().await
        };
        assert_eq!(
            families_that_ran(&allowed),
            EXECUTOR_FAMILIES.to_vec(),
            "without the policy every family has to run, or the fixture is wrong"
        );

        let blocked = {
            let _managed =
                test_support::ManagedSettingsGuard::install(Some(r#"{"disableAllHooks": true}"#));
            run_every_executor_family().await
        };
        assert_eq!(
            families_that_ran(&blocked),
            Vec::<&str>::new(),
            "a managed disableAllHooks policy must stop EVERY executor family"
        );
    }

    /// Maps to: CC `utils/hooks.ts:1994-1999` and `:3031-3036` —
    /// `shouldSkipHookDueToTrust()`, whose comment is "SECURITY: ALL hooks
    /// require workspace trust in interactive mode. This centralized check
    /// prevents RCE vulnerabilities for all current and future hooks".
    ///
    /// CC's granularity is PER-CALL, not per-hook: the check sits at executor
    /// entry, ahead of `getMatchingHooks` (`:2004`, `:3041`), so it never sees
    /// individual entries and cannot filter them. This test pins that by
    /// configuring TWO hooks per event and asserting the untrusted run produces
    /// ZERO results rather than a filtered subset — the behaviour that
    /// distinguishes it from `shouldAllowManagedHooksOnly()`, which really is a
    /// per-source filter applied at merge time (`hooks.ts:1516`, `:1541`).
    ///
    /// Old shape: no port executor had a trust gate except
    /// `env::execute_hooks_outside_repl_with_config` and the compaction pair,
    /// so the untrusted half FAILED naming the other twelve families. Same
    /// again for the sixteenth (`query::stop_hooks`) one batch later: an
    /// untrusted workspace ran Stop hooks through the progress path, which is
    /// the RCE surface `hooks.ts:1992-1993` names, on the one executor a user
    /// reaches at the end of literally every turn.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn untrusted_workspace_stops_every_executor_family_without_filtering() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed = test_support::ManagedSettingsGuard::install(None);

        let trusted = {
            let _trust = test_support::SessionTrustGuard::accepted();
            run_every_executor_family().await
        };
        assert_eq!(
            families_that_ran(&trusted),
            EXECUTOR_FAMILIES.to_vec(),
            "a trusted workspace runs every family, or the fixture is wrong"
        );

        let untrusted = {
            let _denied = test_support::WorkspaceTrustDeniedGuard::install();
            run_every_executor_family().await
        };
        assert_eq!(
            families_that_ran(&untrusted),
            Vec::<&str>::new(),
            "an untrusted workspace must stop EVERY executor family"
        );
    }

    /// The per-call/per-hook distinction, stated on one executor with a config
    /// that mixes two entries: an untrusted call runs NEITHER, where a per-hook
    /// gate would have to run one of them or none for a per-hook reason. CC has
    /// no per-hook trust attribute at all — `shouldSkipHookDueToTrust()` takes
    /// no arguments (`hooks.ts:286`) and is evaluated once per `executeHooks`
    /// call, before `matchingHooks` exists.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn trust_gate_is_evaluated_once_per_call_not_once_per_hook() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed = test_support::ManagedSettingsGuard::install(None);

        let config: HooksConfig = serde_json::from_value(serde_json::json!({
            "UserPromptSubmit": [
                {"hooks": [{"command": "printf first", "timeout": 5}]},
                {"hooks": [{"command": "printf second", "timeout": 5}]}
            ]
        }))
        .unwrap();
        let config = test_support::registered_config(&config);

        let trusted = {
            let _trust = test_support::SessionTrustGuard::accepted();
            prompt::execute_user_prompt_submit_hooks(&config, "a prompt", "default", vec![]).await
        };
        assert_eq!(trusted.len(), 2, "both entries match and run when trusted");

        let untrusted = {
            let _denied = test_support::WorkspaceTrustDeniedGuard::install();
            prompt::execute_user_prompt_submit_hooks(&config, "a prompt", "default", vec![]).await
        };
        assert!(
            untrusted.is_empty(),
            "the whole call is skipped, not filtered entry by entry: {:?}",
            untrusted.len()
        );
    }

    /// The harness invariant every hook test now depends on, asserted ONCE and
    /// by name so a broken assumption produces one legible failure instead of
    /// dozens of "expected N results, got 0" across the suite.
    ///
    /// `should_skip_hook_execution`'s trust arm makes "is this workspace
    /// trusted?" an input to every hook test. Under `just test` the answer is
    /// yes because `just _prep` rebuilds mutable `target/test-home` from tracked
    /// fixtures and serializes `.claude.json` with exactly the canonical
    /// `tests/fixtures/isolated-project` trust key that
    /// `COMETIX_TEST_PROJECT_DIR` pins as the original cwd.
    ///
    /// If this test fails, rerun `just _prep`; it recreates the canonical trust
    /// JSON and tracked mock credentials. Do NOT respond by adding
    /// `SessionTrustGuard` to whatever else went red: a per-test opt-in that
    /// silently makes hooks run is the same trap in the other direction. The
    /// guard belongs only in tests that deliberately relocate the config root
    /// or cwd (`main.rs`, `utils/session_start.rs`) and therefore opt out of the
    /// harness default on purpose.
    #[test]
    fn the_test_harness_workspace_is_trusted_so_hook_tests_are_not_silently_vacuous() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        // Read the real harness state, not a latched answer from earlier in this
        // process (`utils/config.rs:1784-1795` caches `true` forever).
        crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
        assert!(
            !crate::bootstrap::state::get_session_trust_accepted(),
            "no SessionTrustGuard may be live here — this test measures the harness, not a fixture"
        );
        assert!(
            crate::utils::config::check_has_trust_dialog_accepted(),
            "the `just test` workspace is untrusted, so every ungarded hook test \
             is now asserting on the skip path; see this test's doc comment. \
             original_cwd={:?}",
            crate::bootstrap::state::get_original_cwd()
        );
    }

    /// Maps to: CC `utils/hooks.ts:1492-1566#getHooksConfig` — the third arm
    /// (`:1541-1563`) merges `getSessionHooks(appState, sessionId, hookEvent)`,
    /// and `:1516` + `:1534-1541` skip it entirely under
    /// `shouldAllowManagedHooksOnly()`.
    #[test]
    fn session_hooks_join_the_settings_and_registered_channels_for_the_given_id() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed = test_support::ManagedSettingsGuard::install(None);
        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook(
            "agent-merge",
            HookEvent::Stop,
            "",
            HookCommand {
                command: "echo agent stop".to_string(),
                shell: None,
                timeout: Some(5),
                condition: None,
                status: None,
                once: None,
                is_async: None,
                async_rewake: None,
            },
        );

        let merged = load_hooks_config_with_session_hooks("agent-merge");
        let other = load_hooks_config_with_session_hooks("someone-else");
        session_hooks::clear_all_session_hooks();

        assert_eq!(
            merged
                .get("Stop")
                .map(|entries| entries.len())
                .unwrap_or_default(),
            1,
            "the id's own session hook has to reach the executor's table"
        );
        assert!(
            other.get("Stop").is_none_or(Vec::is_empty),
            "another id's hooks must not leak in (CC scopes by sessionId, :1542)"
        );
    }

    /// The managed-only gate CC applies before the session arm (`:1516`,
    /// `:1534-1541`: "Skip session hooks entirely when allowManagedHooksOnly is
    /// set — this prevents frontmatter hooks from agents/skills from bypassing
    /// the policy").
    #[test]
    fn allow_managed_hooks_only_policy_drops_the_session_arm() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed =
            test_support::ManagedSettingsGuard::install(Some(r#"{"allowManagedHooksOnly": true}"#));
        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook(
            "agent-managed-only",
            HookEvent::Stop,
            "",
            HookCommand {
                command: "echo agent stop".to_string(),
                shell: None,
                timeout: Some(5),
                condition: None,
                status: None,
                once: None,
                is_async: None,
                async_rewake: None,
            },
        );

        let merged = load_hooks_config_with_session_hooks("agent-managed-only");
        session_hooks::clear_all_session_hooks();

        assert!(
            merged.get("Stop").is_none_or(Vec::is_empty),
            "a managed-only policy has to stop the session arm"
        );
    }

    /// One row per hook event, transcribed from the schema that DEFINES its
    /// wire shape: `entrypoints/sdk/coreSchemas.ts`. `BaseHookInputSchema`
    /// (`:387-411`) contributes `session_id`/`transcript_path`/`cwd` as required
    /// and `permission_mode`/`agent_id`/`agent_type` as optional to every row,
    /// because every builder spreads `createBaseHookInput` (`hooks.ts:301-328`).
    struct EventSchema {
        event: &'static str,
        /// Event-specific keys the schema declares without `.optional()`.
        required: &'static [&'static str],
        /// Event-specific keys declared `.optional()`.
        optional: &'static [&'static str],
    }

    const BASE_REQUIRED: &[&str] = &["session_id", "transcript_path", "cwd"];
    const BASE_OPTIONAL: &[&str] = &["permission_mode", "agent_id", "agent_type"];

    fn assert_key_set(input: &serde_json::Value, schema: &EventSchema) {
        let object = input.as_object().expect("hook input is an object");
        let allowed: Vec<&str> = BASE_REQUIRED
            .iter()
            .chain(BASE_OPTIONAL.iter())
            .chain(schema.required.iter())
            .chain(schema.optional.iter())
            .copied()
            .collect();
        for key in object.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "{}: `{key}` is not in its coreSchemas.ts key set {allowed:?}",
                schema.event
            );
        }
        for key in BASE_REQUIRED.iter().chain(schema.required.iter()) {
            assert!(
                object.contains_key(*key),
                "{}: required key `{key}` is missing from {:?}",
                schema.event,
                object.keys().collect::<Vec<_>>()
            );
        }
        assert_eq!(
            object.get("hook_event_name").and_then(|v| v.as_str()),
            Some(schema.event),
            "hook_event_name must be the event's own literal"
        );
    }

    /// The deliverable of the #188 audit: every builder's emitted key set is
    /// checked against its CC schema in BOTH directions — no key CC does not
    /// send, and no required key missing — so a future builder cannot drift
    /// silently.
    ///
    /// Two drifts this pins, both found by the audit:
    /// - PermissionRequest used to emit `tool_use_id`. That key belongs to
    ///   `PreToolUseHookInputSchema` (`coreSchemas.ts:420`) and CC writes it only
    ///   there (`hooks.ts:3423`); `PermissionRequestHookInputSchema`
    ///   (`:425-434`) has no such field.
    /// - The tool/lifecycle/task/teammate/prompt builders emitted NO base at
    ///   all, so `session_id`, `transcript_path` and `cwd` — the three keys CC
    ///   guarantees on every hook input — were absent from the JSON a hook
    ///   script reads. `Stop` and `SubagentStop` were additionally missing the
    ///   required `stop_hook_active`.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn emitted_hook_input_key_sets_match_the_official_schemas() {
        use crate::types::permissions::PermissionMode;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        // No policy: the tool-event executor now consults the managed gate, and
        // the developer's real managed root must not decide this test.
        let _managed = test_support::ManagedSettingsGuard::install(None);
        // …and every executor now consults the trust gate too, so the fixture
        // states trust rather than inheriting it from the scratch `.claude.json`
        // (which is gitignored, i.e. not a contract).
        let _trust = test_support::SessionTrustGuard::accepted();

        let tool_input = serde_json::json!({"file_path": "/tmp/source.txt"});

        // ── tools/ (utils/hooks.ts:3394-3562, coreSchemas.ts:414-471) ────────
        let capture = test_support::HookInputCapture::new();
        pre_tool::execute_pre_tool_hooks(
            &capture.config("PreToolUse", Some("Read")),
            "Read",
            "toolu_pre",
            &tool_input,
            &HookContext::default(),
            capture.base_env(),
            None,
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "PreToolUse",
                required: &["hook_event_name", "tool_name", "tool_input", "tool_use_id"],
                optional: &[],
            },
        );

        let capture = test_support::HookInputCapture::new();
        tool::execute_post_tool_hooks(
            &capture.config("PostToolUse", Some("Read")),
            "Read",
            "toolu_post",
            &tool_input,
            &serde_json::json!({"type": "text"}),
            &HookContext::default(),
            capture.base_env(),
            None,
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "PostToolUse",
                required: &[
                    "hook_event_name",
                    "tool_name",
                    "tool_input",
                    "tool_response",
                    "tool_use_id",
                ],
                optional: &[],
            },
        );

        let capture = test_support::HookInputCapture::new();
        tool::execute_post_tool_use_failure_hooks(
            &capture.config("PostToolUseFailure", Some("Read")),
            "Read",
            "toolu_fail",
            &tool_input,
            "boom",
            // `None` is the `undefined` half of CC's `isInterrupt?: boolean`
            // — the key-set row below declares `is_interrupt` optional, and
            // omission is what an absent parameter has to produce.
            None,
            &HookContext::default(),
            capture.base_env(),
            None,
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "PostToolUseFailure",
                required: &[
                    "hook_event_name",
                    "tool_name",
                    "tool_input",
                    "tool_use_id",
                    "error",
                ],
                optional: &["is_interrupt"],
            },
        );

        let capture = test_support::HookInputCapture::new();
        tool::execute_permission_denied_hooks(
            &capture.config("PermissionDenied", Some("Read")),
            "Read",
            "toolu_denied",
            &tool_input,
            "Permission denied",
            &HookContext::default(),
            capture.base_env(),
            None,
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "PermissionDenied",
                required: &[
                    "hook_event_name",
                    "tool_name",
                    "tool_input",
                    "tool_use_id",
                    "reason",
                ],
                optional: &[],
            },
        );

        let capture = test_support::HookInputCapture::new();
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-keyset".to_string(),
            "toolu_keyset".to_string(),
            "Read".to_string(),
            "/tmp/source.txt".to_string(),
            tool_input.clone(),
            PermissionMode::Default,
        );
        permission_request::execute_permission_request_hooks(
            &capture.config("PermissionRequest", Some("Read")),
            &request,
            capture.base_env(),
            None,
        )
        .await;
        let permission_request_input = capture.read();
        assert_key_set(
            &permission_request_input,
            &EventSchema {
                event: "PermissionRequest",
                required: &["hook_event_name", "tool_name", "tool_input"],
                optional: &["permission_suggestions"],
            },
        );
        assert!(
            permission_request_input.get("tool_use_id").is_none(),
            "PermissionRequest has no tool_use_id (coreSchemas.ts:425-434)"
        );

        // ── lifecycle (utils/hooks.ts:3570-4117) ─────────────────────────────
        let capture = test_support::HookInputCapture::new();
        lifecycle::execute_notification_hooks(
            &capture.config("Notification", Some("permission")),
            "needs your approval",
            "permission",
            None,
            capture.base_env(),
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "Notification",
                required: &["hook_event_name", "message", "notification_type"],
                optional: &["title"],
            },
        );

        let capture = test_support::HookInputCapture::new();
        lifecycle::execute_stop_hooks(
            &capture.config("Stop", None),
            Some("default"),
            false,
            Some("final answer"),
            capture.base_env(),
            None,
            None,
            None,
            None,
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "Stop",
                required: &["hook_event_name", "stop_hook_active"],
                optional: &["last_assistant_message"],
            },
        );

        let capture = test_support::HookInputCapture::new();
        lifecycle::execute_stop_failure_hooks(
            &capture.config("StopFailure", None),
            "API boom",
            Some("details"),
            Some("partial"),
            capture.base_env(),
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "StopFailure",
                required: &["hook_event_name", "error"],
                optional: &["error_details", "last_assistant_message"],
            },
        );

        let capture = test_support::HookInputCapture::new();
        lifecycle::execute_session_start_hooks(
            &capture.config("SessionStart", Some("resume")),
            "resume",
            Some("target-session"),
            Some("reviewer"),
            Some("claude-sonnet"),
            capture.base_env(),
            None,
        )
        .await;
        let session_start_input = capture.read();
        assert_key_set(
            &session_start_input,
            &EventSchema {
                event: "SessionStart",
                required: &["hook_event_name", "source"],
                optional: &["agent_type", "model"],
            },
        );
        assert_eq!(
            session_start_input
                .get("session_id")
                .and_then(|v| v.as_str()),
            Some("target-session"),
            "CC createBaseHookInput(undefined, sessionId) honours the override (hooks.ts:3877)"
        );

        let capture = test_support::HookInputCapture::new();
        lifecycle::execute_setup_hooks(
            &capture.config("Setup", Some("init")),
            "init",
            capture.base_env(),
            None,
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "Setup",
                required: &["hook_event_name", "trigger"],
                optional: &[],
            },
        );

        let capture = test_support::HookInputCapture::new();
        lifecycle::execute_session_end_hooks(
            &capture.config("SessionEnd", None),
            "clear",
            capture.base_env(),
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "SessionEnd",
                required: &["hook_event_name", "reason"],
                optional: &[],
            },
        );

        // ── outside-REPL config watcher (utils/hooks.ts:4214-4239) ───────────
        let capture = test_support::HookInputCapture::new();
        env::execute_config_change_hooks_with_config(
            &capture.config("ConfigChange", Some("user_settings")),
            env::ConfigChangeSource::UserSettings,
            Some("/repo/.claude/settings.json"),
            create_base_hook_input(&HookContext::default()),
            capture.base_env(),
            5_000,
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "ConfigChange",
                required: &["hook_event_name", "source"],
                optional: &["file_path"],
            },
        );

        // ── prompt / task / teammate ─────────────────────────────────────────
        let capture = test_support::HookInputCapture::new();
        prompt::execute_user_prompt_submit_hooks(
            &capture.config("UserPromptSubmit", None),
            "a prompt",
            "default",
            capture.base_env(),
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "UserPromptSubmit",
                required: &["hook_event_name", "prompt"],
                optional: &[],
            },
        );

        let task_schema = |event| EventSchema {
            event,
            required: &["hook_event_name", "task_id", "task_subject"],
            optional: &["task_description", "teammate_name", "team_name"],
        };

        let capture = test_support::HookInputCapture::new();
        task::execute_task_created_hooks(
            &capture.config("TaskCreated", None),
            "task-1",
            "Review auth",
            Some("Inspect auth flow"),
            None,
            None,
            capture.base_env(),
        )
        .await;
        assert_key_set(&capture.read(), &task_schema("TaskCreated"));

        let capture = test_support::HookInputCapture::new();
        task::execute_task_completed_hooks(
            &capture.config("TaskCompleted", None),
            "task-1",
            "Review auth",
            None,
            None,
            None,
            capture.base_env(),
        )
        .await;
        assert_key_set(&capture.read(), &task_schema("TaskCompleted"));

        let capture = test_support::HookInputCapture::new();
        teammate::execute_teammate_idle_hooks(
            &capture.config("TeammateIdle", None),
            "ada",
            "alpha",
            Some("default"),
            capture.base_env(),
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "TeammateIdle",
                required: &["hook_event_name", "teammate_name", "team_name"],
                optional: &[],
            },
        );

        let capture = test_support::HookInputCapture::new();
        teammate::execute_subagent_start_hooks(
            &capture.config("SubagentStart", Some("general-purpose")),
            "agent-1",
            "general-purpose",
            capture.base_env(),
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "SubagentStart",
                required: &["hook_event_name", "agent_id", "agent_type"],
                optional: &[],
            },
        );

        let capture = test_support::HookInputCapture::new();
        teammate::execute_subagent_stop_hooks(
            &capture.config("SubagentStop", Some("general-purpose")),
            "agent-1",
            "general-purpose",
            "/tmp/agent-agent-1.jsonl",
            Some("final answer"),
            capture.base_env(),
        )
        .await;
        assert_key_set(
            &capture.read(),
            &EventSchema {
                event: "SubagentStop",
                required: &[
                    "hook_event_name",
                    "stop_hook_active",
                    "agent_id",
                    "agent_transcript_path",
                    "agent_type",
                ],
                optional: &["last_assistant_message"],
            },
        );
    }

    /// `JSON.stringify` drops an object property whose value is `undefined`, so
    /// an optional CC does not set sends NO key. Emitting `"title": null`
    /// instead is a different payload for any hook that branches on key
    /// presence, and the old builders emitted exactly that.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn absent_optionals_send_no_key_rather_than_null() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed = test_support::ManagedSettingsGuard::install(None);
        let _trust = test_support::SessionTrustGuard::accepted();

        let capture = test_support::HookInputCapture::new();
        lifecycle::execute_stop_failure_hooks(
            &capture.config("StopFailure", None),
            "API boom",
            None,
            None,
            capture.base_env(),
        )
        .await;
        let input = capture.read();

        assert!(input.get("error_details").is_none(), "got {input}");
        assert!(input.get("last_assistant_message").is_none(), "got {input}");
        // …and the base's own optionals behave the same way.
        assert!(input.get("agent_id").is_none(), "got {input}");
        assert!(input.get("permission_mode").is_none(), "got {input}");
    }

    /// `createBaseHookInput` (`hooks.ts:301-328`) returns SIX keys.
    /// `project_dir` and `claude_code_version` were port-side additions: CC's
    /// only `project_dir` is nested under the StatusLine payload's `workspace`
    /// (`components/StatusLine.tsx:118-122`) and `claude_code_version` belongs
    /// to the SDK `system.init` message (`utils/messages/systemInit.ts:74`).
    #[test]
    fn base_hook_input_carries_the_official_six_keys_only() {
        let base = create_base_hook_input(&HookContext {
            session_id: "session-1".to_string(),
            transcript_path: "/tmp/session-1.jsonl".to_string(),
            cwd: "/repo".to_string(),
            permission_mode: Some("default".to_string()),
            agent_id: Some("agent-1".to_string()),
            agent_type: Some("reviewer".to_string()),
        });
        let mut keys: Vec<&str> = base
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "agent_id",
                "agent_type",
                "cwd",
                "permission_mode",
                "session_id",
                "transcript_path",
            ]
        );
    }

    /// CC resolves the three required base fields from process state when the
    /// caller supplies none — `sessionId ?? getSessionId()` (`:315`),
    /// `getTranscriptPathForSession(...)` (`:322`), `getCwd()` (`:323`) — so a
    /// builder that has no context still sends real values, never `""`.
    #[test]
    fn empty_carrier_fields_fall_back_to_process_state_like_the_official_nullish_arms() {
        let base = create_base_hook_input(&HookContext::default());
        let object = base.as_object().expect("object");
        for key in ["session_id", "transcript_path", "cwd"] {
            assert!(
                object
                    .get(key)
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| !value.is_empty()),
                "{key} must resolve, got {base}"
            );
        }
    }
}
#[cfg(test)]
mod foundation_tests {
    //! Source regressions for the shared permission/hook foundation.
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::time::Duration;

    fn parsed(value: Value) -> HookResult {
        let parsing::ParsedHookOutput::Json(json) = parsing::parse_hook_output(&value.to_string())
        else {
            panic!("fixture must pass the actual hook output schema");
        };
        parsing::process_hook_json_output(&json, "fixture")
    }

    #[test]
    fn json_decisions_match_official_separate_stop_block_and_permission_fields() {
        // CC hooks.ts:518-523, 2607-2611: continue:false isn't a blocking error.
        let stop = parsed(json!({"continue": false, "stopReason": ""}));
        assert!(stop.prevent_continuation);
        assert!(stop.stop_reason.is_none());
        assert!(stop.blocking_error.is_none());
        assert_eq!(stop.outcome, HookOutcome::Success);

        // CC :532-538 and :615-624 use JS truthiness for the block copy.
        let block = parsed(json!({"decision": "block", "reason": ""}));
        assert_eq!(
            block.blocking_error.unwrap().blocking_error,
            "Blocked by hook"
        );
        assert_eq!(block.outcome, HookOutcome::Success);
        for (specific, top, expected) in [
            (Some("specific"), Some("top"), "specific"),
            (Some(""), Some("top"), "top"),
            (None, Some(""), "Blocked by hook"),
        ] {
            let mut value = json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse", "permissionDecision": "deny"
            }});
            if let Some(reason) = top {
                value["reason"] = json!(reason);
            }
            if let Some(reason) = specific {
                value["hookSpecificOutput"]["permissionDecisionReason"] = json!(reason);
            }
            let deny = parsed(value);
            assert_eq!(deny.blocking_error.unwrap().blocking_error, expected);
            assert_eq!(deny.hook_permission_decision_reason.as_deref(), specific);
            assert_eq!(deny.outcome, HookOutcome::Success);
        }

        // CC :631-632 includes ask and overwrites the top-level reason.
        let ask = parsed(json!({"reason": "top", "hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "ask",
            "permissionDecisionReason": "ask reason"
        }}));
        assert_eq!(
            ask.hook_permission_decision_reason.as_deref(),
            Some("ask reason")
        );

        // CC :657-672 keeps deny message/interrupt solely on the nested decision.
        let deny = parsed(json!({"hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "deny", "message": "policy", "interrupt": true}
        }}));
        assert!(deny.blocking_error.is_none());
        assert!(!deny.prevent_continuation);
        assert_eq!(deny.outcome, HookOutcome::Success);
        assert!(matches!(
            deny.permission_request_result,
            Some(crate::types::hooks::PermissionRequestResult::Deny {
                interrupt: Some(true),
                ..
            })
        ));
    }

    fn immediate(value: Value) -> RegisteredHook {
        RegisteredHook::Callback(HookCallback {
            callback: Arc::new(move |_, _| {
                let value = value.clone();
                Box::pin(async move { value })
            }),
            timeout: Some(3),
        })
    }

    fn config(event: HookEvent, hooks: Vec<RegisteredHook>) -> RegisteredHooks {
        std::collections::HashMap::from([(
            event.as_str().to_string(),
            vec![RegisteredHookMatcher {
                matcher: Some("Read".into()),
                hooks,
                ..Default::default()
            }],
        )])
    }

    fn request() -> crate::types::permissions::PermissionRequest {
        crate::utils::permissions::permissions::mock_permission_request_with_input(
            "foundation",
            "toolu_foundation",
            "Read",
            "/tmp/foundation",
            json!({"file_path": "/tmp/foundation"}),
            crate::types::permissions::PermissionMode::Default,
        )
    }

    #[tokio::test]
    async fn hooks_match_official_parallel_completion_and_sticky_permission_precedence() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _trust = test_support::SessionTrustGuard::accepted();
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let (sender, receiver) = async_channel::bounded::<()>(1);
        let waiting = RegisteredHook::Callback(HookCallback {
            callback: Arc::new(move |_, _| {
                let receiver = receiver.clone();
                Box::pin(async move {
                    // A sequential executor cannot complete this handshake.
                    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                        .await
                        .unwrap()
                        .unwrap();
                    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                        "permissionDecision": "deny", "permissionDecisionReason": "late deny"}})
                })
            }),
            timeout: Some(3),
        });
        let signaling = RegisteredHook::Callback(HookCallback {
            callback: Arc::new(move |_, _| {
                let sender = sender.clone();
                Box::pin(async move {
                    sender.send(()).await.unwrap();
                    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                        "permissionDecision": "allow", "permissionDecisionReason": "early allow"}})
                })
            }),
            timeout: Some(3),
        });
        // CC hooks.ts:2143/2739 plus generators.ts all: completion order, not input order.
        let results = pre_tool::execute_pre_tool_hooks(
            &config(HookEvent::PreToolUse, vec![waiting, signaling]),
            "Read",
            "toolu_foundation",
            &json!({}),
            &HookContext::default(),
            vec![],
            None,
        )
        .await;
        assert_eq!(
            results[0].hook_permission_decision_reason.as_deref(),
            Some("early allow")
        );
        assert_eq!(
            results[1].permission_behavior,
            Some(PermissionBehavior::Deny)
        );

        // Source starts siblings even when the first completed JSON asks to stop.
        let results = pre_tool::execute_pre_tool_hooks(&config(HookEvent::PreToolUse, vec![
            immediate(json!({"continue": false})),
            immediate(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "ask"}})),
        ]), "Read", "toolu_siblings", &json!({}), &HookContext::default(), vec![], None).await;
        assert!(results.iter().any(|result| result.prevent_continuation));
        assert!(
            results
                .iter()
                .any(|result| result.permission_behavior == Some(PermissionBehavior::Ask))
        );
    }

    #[tokio::test]
    async fn permission_request_matches_official_first_nested_decision_and_winner_payload() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _trust = test_support::SessionTrustGuard::accepted();
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let config = config(
            HookEvent::PermissionRequest,
            vec![
                immediate(
                    json!({"decision": "block", "reason": "not a PermissionRequest decision"}),
                ),
                immediate(
                    json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {
                        "behavior": "allow", "updatedInput": {"file_path": "/tmp/winner"},
                        "updatedPermissions": [{"type": "setMode", "mode": "acceptEdits", "destination": "session"}]
                    }}}),
                ),
                immediate(
                    json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {
                        "behavior": "deny", "message": "late denial", "interrupt": true
                    }}}),
                ),
            ],
        );
        let abort = crate::tool::AbortController::default();
        let result = crate::services::tools::tool_hooks::run_permission_request_hooks_with_config(
            &request(),
            &config,
            &HookContext::default(),
            vec![],
            Some(&abort),
        )
        .await;
        // PermissionContext.ts:230-262: no ranking, no later input/update merge.
        assert_eq!(result.permission_behavior, Some(PermissionBehavior::Allow));
        assert_eq!(
            result.updated_input,
            Some(json!({"file_path": "/tmp/winner"}))
        );
        assert_eq!(result.permission_updates.len(), 1);
        assert!(!result.prevent_continuation);
        assert!(
            !abort.is_aborted(),
            "the losing denial cannot interrupt the winner"
        );
    }

    #[tokio::test]
    async fn pre_tool_input_only_completion_matches_official_independent_yield() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _trust = test_support::SessionTrustGuard::accepted();
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let config = config(
            HookEvent::PreToolUse,
            vec![
                immediate(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                "permissionDecision": "ask", "updatedInput": {"file_path": "/tmp/first"}}})),
                immediate(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                "updatedInput": {"file_path": "/tmp/input-only"}}})),
            ],
        );
        let results = pre_tool::execute_pre_tool_hooks(
            &config,
            "Read",
            "toolu_input_only",
            &json!({}),
            &HookContext::default(),
            vec![],
            None,
        )
        .await;
        // CC hooks.ts:2855-2880 emits the accumulated decision, THEN an independent
        // updatedInput yield when the completing hook supplied no decision itself.
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[1].permission_behavior,
            Some(PermissionBehavior::Ask)
        );
        assert_eq!(results[1].updated_input, None);
        assert_eq!(results[2].permission_behavior, None);
        assert_eq!(
            results[2].updated_input,
            Some(json!({"file_path": "/tmp/input-only"}))
        );
        assert_eq!(results[2].command, None);
        let seam = crate::services::tools::tool_hooks::run_pre_tool_use_hooks(
            &request(),
            &config,
            &HookContext::default(),
            vec![],
            None,
        )
        .await;
        assert_eq!(seam.permission_behavior, Some(PermissionBehavior::Ask));
        assert_eq!(
            seam.updated_input,
            Some(json!({"file_path": "/tmp/input-only"}))
        );
    }

    #[tokio::test]
    async fn bare_mode_matches_official_gate_before_any_tool_hook_executes() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _trust = test_support::SessionTrustGuard::accepted();
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let config = config(
            HookEvent::PreToolUse,
            vec![RegisteredHook::Callback(HookCallback {
                callback: Arc::new(|_, _| panic!("bare mode must gate before callback invocation")),
                timeout: Some(3),
            })],
        );
        assert!(
            pre_tool::execute_pre_tool_hooks(
                &config,
                "Read",
                "toolu_bare",
                &json!({}),
                &HookContext::default(),
                vec![],
                None
            )
            .await
            .is_empty()
        );
    }
    #[test]
    fn base_hook_transcript_path_matches_official_resolved_session_id() {
        // hooks.ts:317-324 + sessionStorage.ts:207-228: current honors active
        // project dir, other IDs use originalCwd, explicit native override wins.
        let _env = TEST_ENV_LOCK.lock().unwrap();
        struct Restore(String, Option<std::path::PathBuf>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(self.0.clone(), self.1.clone());
            }
        }
        let _restore = Restore(
            crate::bootstrap::state::get_session_id(),
            crate::bootstrap::state::get_session_project_dir(),
        );
        crate::bootstrap::state::switch_session("hook-active", Some("/active-project".into()));
        let active = create_base_hook_input_object(&HookContext::default());
        assert_eq!(
            active["transcript_path"],
            "/active-project/hook-active.jsonl"
        );
        let target = create_base_hook_input_object(&HookContext {
            session_id: "hook-target".into(),
            ..Default::default()
        });
        assert_eq!(target["session_id"], "hook-target");
        assert_eq!(
            target["transcript_path"],
            crate::utils::session_storage::get_session_file_path(
                &crate::bootstrap::state::get_original_cwd()
                    .display()
                    .to_string(),
                "hook-target"
            )
            .display()
            .to_string()
        );
        let explicit = create_base_hook_input_object(&HookContext {
            session_id: "hook-target".into(),
            transcript_path: "/explicit/path.jsonl".into(),
            ..Default::default()
        });
        assert_eq!(explicit["transcript_path"], "/explicit/path.jsonl");
    }
}

#[cfg(test)]
mod plugin_hook_event_order_tests {
    use super::*;
    #[test]
    fn plugin_hook_event_order_matches_official_bun_constant() {
        let expected: Vec<String> = serde_json::from_str(r###"["PreToolUse","PostToolUse","PostToolUseFailure","Notification","UserPromptSubmit","SessionStart","SessionEnd","Stop","StopFailure","SubagentStart","SubagentStop","PreCompact","PostCompact","PermissionRequest","PermissionDenied","Setup","TeammateIdle","TaskCreated","TaskCompleted","Elicitation","ElicitationResult","ConfigChange","WorktreeCreate","WorktreeRemove","InstructionsLoaded","CwdChanged","FileChanged"]"###).unwrap();
        assert_eq!(
            HOOK_EVENTS
                .iter()
                .map(|event| event.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
}
