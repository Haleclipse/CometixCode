//! Maps to: CC `screens/REPL.tsx` — the conversation screen/use-case owner.
//! CC data ownership:
//!   REPL owns: messages, screen state, tools, commands
//!   PromptInput owns: input, exitMessage, suggestions, cursor
//!   App (ink) owns: providers, error boundary, stdin/raw mode
//! Exit flow: PromptInput.on_exit → REPL → App.exit()

use crate::commands::agents::agents::AgentsCommand;
use crate::commands::btw::BtwSideQuestion;
use crate::commands::copy::copy::CopyPicker;
use crate::commands::diff::diff::DiffCommand;
use crate::commands::hooks::hooks::HooksCommand;
use crate::commands::resume::resume::ResumeCommand;
use crate::commands::skills::skills::SkillsCommand;
use crate::commands::{doctor, effort, fast, mcp, memory, model, permissions, resume, theme};
use crate::components::bash_mode_progress::BashModeProgress;
use crate::components::desktop_handoff::DesktopHandoffState;
use crate::components::desktop_upsell::{
    DesktopUpsellDone, DesktopUpsellStartup, DesktopUpsellStartupSnapshot,
    get_desktop_upsell_config, should_show_desktop_upsell_startup,
};
use crate::components::diff::DiffDialogDone;
use crate::components::effort_picker::EffortPicker;
use crate::components::exit_flow::{ExitFlow, ExitFlowDone};
use crate::components::export_dialog::{ExportDialog, ExportDialogResult};
use crate::components::help_v2::HelpV2;
use crate::components::hooks::HooksConfigMenuDone;
use crate::components::mcp::elicitation_dialog::ElicitationDialog;
use crate::components::messages_list::Messages;
use crate::components::permissions::{
    PermissionRequest, SandboxPermissionRequest, WorkerPendingPermission,
};
use crate::components::prompt_input::{PromptInput, PromptSubmission};
use crate::components::remote_callout::{
    RemoteCallout, RemoteCalloutDone, RemoteCalloutSnapshot, should_show_remote_callout,
};
use crate::components::sandbox::SandboxSettings;
use crate::components::settings::{Settings, SettingsTab};
use crate::components::skills::skills_menu::SkillsMenuDone;
use crate::components::spinner::{SpinnerMode, SpinnerWithVerb};
use crate::components::stats::Stats;
use crate::hooks::tool_permission::handlers::interactive_handler::tool_use_confirm_for_request;
use crate::hooks::use_api_key_verification::use_api_key_verification;
use crate::hooks::use_ide_selection::IdeSelection;
use crate::query::{
    PendingQueryMessage, QueryCommand, QueryEvent, QueryHandle, StopHookProgressEvent,
    deps::production_deps, spawn_query,
};
use crate::services::mcp::client::{McpPromptCommandSnapshot, respond_to_mcp_elicitation};
use crate::services::mcp::elicitation_handler::{
    ElicitationAction, ElicitationRequestParams, ElicitationWaitingDismissAction,
    url_retry_waiting_dismiss_result,
};
use crate::services::mcp::types::McpPromptSnapshot;
use crate::services::tools::tool_execution::{
    check_permissions_and_call_tool, check_permissions_and_call_tool_with_response,
};
use crate::services::tools::tool_orchestration::run_tools_for_message;
use crate::tool::ToolPermissionContext;
use crate::tool::ToolUseContext;
use crate::types::command::ResumeEntrypoint;
use crate::types::message::Message;
use crate::types::message::UserContent;
use crate::types::message::{
    HistoryEntry, RenderableMessage, RenderableMessageKind, SystemMessage, SystemMessageLevel,
};
use crate::types::permissions::{PermissionPromptChoice, PermissionPromptResponse, ToolUseConfirm};
use crate::utils::classifier_approvals::{ClassifierApprovalsState, ClassifierChecking};
use crate::utils::config::GlobalConfig;
use crate::utils::handle_prompt_submit::{HandlePromptSubmit, HandlePromptSubmitResult};
use crate::utils::messages::{history_model_messages, project_history_rows};
use crate::utils::permissions::permissions::{apply_prompt_choice, apply_prompt_response};
use crate::utils::process_user_input::process_slash_command::{
    LocalCommandUi, LocalSettingsTab, SlashCommandAction, SlashCommandInvocation,
};
use crate::utils::session_restore::ResumeRestoreStores;
use crate::utils::session_storage::SessionSelection;
use crate::utils::status_notice_definitions::StatusNoticeContext;
use crate::utils::swarm::it2_setup_prompt::{It2SetupPrompt, It2SetupPromptRequest};
use iocraft::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Maps to: CC `screens/REPL.tsx:906` `Screen`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Prompt,
    Transcript,
}

/// Maps to: CC `screens/REPL.tsx:858-903` `Props` launch subset, including
/// the full CLI-resume `initial*` package and `mainThreadAgentDefinition`.
///
/// CC's `initialMessages: Message[]` remains `initial_messages`; the Rust-only
/// `initial_renderable_messages` is the loaded conversation's rich transcript
/// rows. Both seed the ONE history state as a dual-carrier prefix
/// (`seed_history_entries`) until the cold model/render producers
/// converge in batch D. The Rust-only `initial_resume_restore_stores` field is
/// the established ToolUseContext state adapter for CC's process/session
/// stores, not AppState.
/// These are owned launch snapshots, separate from mutable
/// [`crate::state::app_state_store::AppState`]. `Default` exists for iocraft's Props
/// construction and provider-less component tests; production startup always
/// supplies values resolved by `main.tsx`-equivalent code.
#[derive(Clone, Debug, PartialEq, Props)]
pub struct ReplProps {
    /// Maps to: CC required `Props.commands` launch snapshot.
    pub commands: Arc<Vec<crate::commands::Command>>,
    /// Maps to: CC required `Props.initialTools` launch snapshot.
    pub initial_tools: Arc<Vec<crate::types::tools::Tool>>,
    /// Maps to: CC `Props.debug` (`debug || debugToStderr` at launch).
    pub debug: bool,
    /// Maps to: CC `Props.disableSlashCommands`.
    pub disable_slash_commands: bool,
    /// Maps to: CC `Props.initialMessages`.
    pub initial_messages: Option<Arc<Vec<Message>>>,
    /// Rich transcript rows of the loaded conversation — the render half of
    /// the dual-carrier history seed (batch-D seam residue).
    pub initial_renderable_messages: Option<Arc<Vec<RenderableMessage>>>,
    /// Maps to: CC `Props.initialFileHistorySnapshots`.
    pub initial_file_history_snapshots:
        Option<Arc<Vec<crate::utils::file_history::FileHistorySnapshot>>>,
    /// Maps to: CC `Props.initialContentReplacements`.
    pub initial_content_replacements:
        Option<Arc<Vec<crate::utils::tool_result_storage::ContentReplacementRecord>>>,
    /// Maps to: CC `Props.initialAgentName`.
    pub initial_agent_name: Option<Arc<str>>,
    /// Maps to: CC `Props.initialAgentColor`.
    pub initial_agent_color: Option<Arc<str>>,
    /// Maps to: CC `Props.systemPrompt`.
    pub system_prompt: Option<Arc<str>>,
    /// Maps to: CC `Props.appendSystemPrompt`.
    pub append_system_prompt: Option<Arc<str>>,
    /// Maps to: CC `Props.mainThreadAgentDefinition`.
    pub main_thread_agent_definition:
        Option<Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>>,
    pub thinking_config: crate::utils::thinking::ThinkingConfig,
    /// L1 adapter for process stores restored before the first query.
    pub initial_resume_restore_stores: Arc<ResumeRestoreStores>,
    /// Maps to: CC `REPL.tsx:6136-6137` `dynamicMcpConfig` /
    /// `isStrictMcpConfig`, the props REPL hands to `<MCPConnectionManager>`.
    /// `None` leaves the manager inert, which is what every non-interactive
    /// mount wants.
    pub mcp_startup: Option<Arc<crate::main::McpStartupConfig>>,
}

impl Default for ReplProps {
    fn default() -> Self {
        Self {
            commands: Arc::new(Vec::new()),
            initial_tools: Arc::new(Vec::new()),
            debug: false,
            disable_slash_commands: false,
            initial_messages: None,
            initial_renderable_messages: None,
            initial_file_history_snapshots: None,
            initial_content_replacements: None,
            initial_agent_name: None,
            initial_agent_color: None,
            system_prompt: None,
            append_system_prompt: None,
            main_thread_agent_definition: None,
            thinking_config: crate::utils::thinking::ThinkingConfig::Adaptive,
            initial_resume_restore_stores: Arc::new(ResumeRestoreStores::default()),
            mcp_startup: None,
        }
    }
}

impl ReplProps {
    /// Apply CC `ProcessedResume` fields before mounting REPL.
    pub fn apply_processed_resume(
        &mut self,
        processed: &crate::utils::session_restore::ProcessedResume,
    ) {
        self.initial_messages = Some(processed.messages.clone());
        self.initial_renderable_messages = Some(processed.renderable_messages.clone());
        self.initial_file_history_snapshots = processed.file_history_snapshots.clone();
        self.initial_content_replacements = processed.content_replacements.clone();
        self.initial_agent_name = processed.agent_name.clone();
        self.initial_agent_color = processed.agent_color.clone();
        self.main_thread_agent_definition = processed.restored_agent_def.clone();
        self.initial_resume_restore_stores = processed.resume_restore_stores.clone();
    }

    /// Rust element-construction equivalent of CC `<REPL {...replProps} />`.
    pub fn into_element(self) -> AnyElement<'static> {
        element! {
            Repl(
                commands: self.commands,
                initial_tools: self.initial_tools,
                debug: self.debug,
                disable_slash_commands: self.disable_slash_commands,
                initial_messages: self.initial_messages,
                initial_renderable_messages: self.initial_renderable_messages,
                initial_file_history_snapshots: self.initial_file_history_snapshots,
                initial_content_replacements: self.initial_content_replacements,
                initial_agent_name: self.initial_agent_name,
                initial_agent_color: self.initial_agent_color,
                system_prompt: self.system_prompt,
                append_system_prompt: self.append_system_prompt,
                main_thread_agent_definition: self.main_thread_agent_definition,
                thinking_config: self.thinking_config,
                initial_resume_restore_stores: self.initial_resume_restore_stores,
                mcp_startup: self.mcp_startup,
            )
        }
        .into_any()
    }
}

/// Maps to CC `screens/REPL.tsx` startup dialog state seeds:
/// `showRemoteCallout` and `showDesktopUpsellStartup`.
///
/// Safety boundary: official REPL computes these by reading app state, global
/// config, GrowthBook, OAuth tokens, and bridge entitlement. Cometix consumes an
/// explicit snapshot context; the read-only producer below never writes config,
/// initializes GrowthBook, opens network/bridge transports, or launches Desktop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplStartupDialogSnapshot {
    pub remote_callout: Option<RemoteCalloutSnapshot>,
    pub desktop_upsell: Option<DesktopUpsellStartupSnapshot>,
    pub desktop_handoff_state: DesktopHandoffState,
    pub desktop_handoff_error: Option<String>,
    pub desktop_handoff_download_message: Option<String>,
}

fn node_platform_from_rust() -> String {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
    .to_string()
}

fn node_arch_from_rust() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
    .to_string()
}

/// Maps to: CC `screens/REPL.tsx` `const mainLoopModel = useMainLoopModel()`.
///
/// AppState may store aliases (`opus`); this returns the API-ready full name.
fn repl_main_loop_model(app_state: &crate::state::app_state_store::AppState) -> String {
    crate::hooks::use_main_loop_model::use_main_loop_model(
        app_state.main_loop_model.as_deref(),
        app_state.main_loop_model_for_session.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_repl_tool_use_context_options(
    context: &mut crate::tool::ToolUseContext,
    callbacks: Option<&crate::services::mcp::channel_permissions::ChannelPermissionCallbacks>,
    interactive_permission_sink: &crate::tool::InteractivePermissionSink,
    fast_mode: bool,
    thinking_enabled: Option<bool>,
    thinking_config: &crate::utils::thinking::ThinkingConfig,
    model: Option<String>,
    effort_value: Option<crate::utils::effort::EffortValue>,
) {
    // Maps to CC wiring `getAppState().channelPermissionCallbacks` / fastMode /
    // thinking / model / effort onto the top-level REPL `toolUseContext` before
    // `query(...)`. `model` must already be API-resolved (CC
    // `useMainLoopModel()`). The launch `thinkingConfig` is required just as it
    // is in CC's REPL Props; only the live AppState enable switch is optional.
    context.channel_permission_callbacks = callbacks.cloned();
    // Maps to CC `REPL.tsx:3409` passing `canUseTool` into `query(...)`. CC's
    // `canUseTool` is a call argument; the port carries its dialog half on the
    // context so a nested `AgentTool` call can reach it the way CC's subagent
    // reaches the same closure (`AgentTool.tsx:399` → `runAgent.ts:753`).
    // Installed here and only here: a headless/forked actor has no REPL behind
    // it, and CC's print mode likewise supplies a `canUseTool` with no dialog
    // leg (`cli/print.ts:4276-4293`).
    context.interactive_permission_sink = interactive_permission_sink.clone();
    // Maps to CC REPL.tsx:3246 getToolUseContext.addNotification. This is
    // the REPL producer; an arbitrary AppStore elsewhere does not supply it.
    context.add_notification =
        crate::tool::AddNotification(context.app_store.store.clone().map(|store| {
            Arc::new(move |notification| {
                crate::context::notifications::NotificationsWriter::new(store.clone())
                    .add_notification(notification);
            }) as Arc<dyn Fn(crate::context::notifications::Notification) + Send + Sync>
        }));
    context.fast_mode = Some(fast_mode);
    // Maps to: CC REPL.tsx
    //   thinkingConfig: s.thinkingEnabled !== false ? thinkingConfig : { type: 'disabled' }
    context.thinking_config = Some(if thinking_enabled == Some(false) {
        crate::utils::thinking::ThinkingConfig::Disabled
    } else {
        thinking_config.clone()
    });
    if let Some(model) = model {
        context.main_loop_model = Some(model);
    }
    // This context is reused across turns. Assign `None` as well so
    // `/effort auto|unset` cannot leave the previous explicit value behind.
    context.effort_value = effort_value;
}

fn repl_response_length_sink(
    response_length_ref: iocraft::prelude::Ref<usize>,
) -> crate::tool::ResponseLengthSink {
    let response_length_ref = std::sync::Arc::new(std::sync::Mutex::new(response_length_ref));
    crate::tool::ResponseLengthSink::new(move |delta| {
        let Ok(mut response_length_ref) = response_length_ref.lock() else {
            return;
        };
        let next = response_length_ref.get().saturating_add(delta);
        response_length_ref.set(next);
    })
}

/// The `Agent(x,y)` frontmatter restriction and the tool pool it narrows are
/// two halves of ONE `resolveAgentTools` call in CC (`REPL.tsx:1203-1221`), and
/// CC's builder installs both on every context it hands out: the pool at
/// `REPL.tsx:3192` (`tools: computeTools()`), the restriction at `:3206-3208`
/// (`agentDefinitions: allowedAgentTypes ? { ...s.agentDefinitions,
/// allowedAgentTypes } : s.agentDefinitions`).
///
/// This port has lost the restriction three times, always the same way — a call
/// site took the pool and dropped the restriction:
///
/// 1. `selected_agent_definition` re-read definitions from disk instead of the
///    request snapshot. Closed by giving the restriction a real carrier field,
///    `AgentDefinitionsResult.allowed_agent_types` (CC `loadAgentsDir.ts:190`).
/// 2. The MCP-prompt slash path built a context the definitions never reached.
///    Closed by [`seed_deferred_query_shell_context`].
/// 3. `ToolUseContext::with_app_store` (`tool.rs:1337`) re-read
///    `agent_definitions` from the store, overwriting a correctly merged value.
///    The store deliberately never holds the overlay — `AppStateStore.ts:505`
///    has no `allowedAgentTypes` — so a bare re-read is always a loss.
///
/// Hence this module. `ReplPromptTools` keeps CC's two field names but keeps
/// them PRIVATE, the producer is private too, and the only way out is
/// [`repl_prompt_tools::install_repl_prompt_tools`], which writes both halves
/// onto a `ToolUseContext` in one call.
///
/// Measured, not asserted — writing
/// `repl_prompt_tools::assemble_repl_prompt_tools(..).tools` in
/// `seed_deferred_query_shell_context` produces TWO errors, either of which
/// alone is enough:
///
/// ```text
/// error[E0603]: function `assemble_repl_prompt_tools` is private
/// error[E0616]: field `tools` of struct `ReplPromptTools` is private
/// ```
///
/// So a fourth recurrence stops compiling instead of silently shipping.
mod repl_prompt_tools {
    use super::ToolUseContext;
    use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionsResult};
    use crate::types::tools::Tool;
    use std::sync::Arc;

    /// Maps to: CC `REPL.tsx:1216-1220` — the inline object the
    /// `resolveAgentTools` memo (`REPL.tsx:1203-1221`) returns. CC's memo yields
    /// BOTH halves of one `resolveAgentTools` call: the narrowed tool pool and
    /// the `Agent(x,y)` restriction parsed out of the same frontmatter spec.
    /// Rust needs a name for the anonymous object; the two fields keep CC's
    /// names.
    struct ReplPromptTools {
        /// Maps to CC `REPL.tsx:1218` `tools: resolved.resolvedTools`.
        tools: Vec<Tool>,
        /// Maps to CC `REPL.tsx:1219`
        /// `allowedAgentTypes: resolved.allowedAgentTypes` (and `:1208`,
        /// `undefined` when there is no main-thread agent).
        allowed_agent_types: Option<Vec<String>>,
    }

    /// Maps to: CC `REPL.tsx:3192` `tools: computeTools()` together with
    /// `:3206-3208` `agentDefinitions: allowedAgentTypes ? {
    /// ...s.agentDefinitions, allowedAgentTypes } : s.agentDefinitions` — the
    /// two lines that land the memo's halves on the context the builder returns.
    ///
    /// `store_definitions` is the caller's `store.getState().agentDefinitions`
    /// read (CC `:3165` `const s = store.getState()`, used at `:3207-3208`), so the
    /// definitions SNAPSHOT stays fresh on every call and only the per-turn
    /// overlay is re-applied on top of it. Permission context and MCP state come
    /// off `context` rather than as parameters, because CC's `computeTools`
    /// likewise reads them fresh at call time (`:3173-3176`) and by the time
    /// this runs they are already the turn's values — the borrow checker then
    /// also makes it impossible to compute the pool from a different permission
    /// context than the one the turn will execute under.
    ///
    /// One deliberate compression of CC's shape: CC's builder takes the pool
    /// from a SECOND `resolveAgentTools` call inside `computeTools` (`:3184`,
    /// keeping only `.resolvedTools`) while the restriction stays
    /// closure-captured from the memo's first call (`:1211`). Both halves come
    /// from one call here, which is equivalent because `allowedAgentTypes` is
    /// parsed straight out of the frontmatter tool spec
    /// (`agentToolUtils.ts:190-194`) before any `availableToolMap` lookup
    /// (`:206`) — it cannot depend on which tool pool the call was given, and
    /// both CC calls read the same `mainThreadAgentDefinition`. Recomputing it
    /// rather than caching it is also what keeps the two impossible to diverge.
    pub(super) fn install_repl_prompt_tools(
        context: &mut ToolUseContext,
        store_definitions: &Arc<AgentDefinitionsResult>,
        initial_tools: &[Tool],
        main_thread_agent_definition: Option<&AgentDefinition>,
    ) {
        let ReplPromptTools {
            tools,
            allowed_agent_types,
        } = assemble_repl_prompt_tools(
            initial_tools,
            &context.tool_permission_context,
            &context.mcp_state,
            main_thread_agent_definition,
        );
        context.tools = tools;
        context.agent_definitions =
            merge_allowed_agent_types(store_definitions, allowed_agent_types);
    }

    /// Maps to: CC `REPL.tsx:5821` / `:6162` `<Messages tools={tools} …>` —
    /// the RENDER consumer of the very same memo (`REPL.tsx:1216`), which takes
    /// the pool half alone because the render tree has no `ToolUseContext` to
    /// install the restriction onto. `Messages.tsx:251` → `MessageRow.tsx:201`
    /// → `Message.tsx:250` → `GroupedToolUseContent.tsx:16` →
    /// `AgentTool/UI.tsx:1096` `findToolByName(tools, name)`.
    ///
    /// Not a hole in the guard above: the return type is `Arc<Vec<Tool>>`, so
    /// `context.tools = repl_render_tool_pool(..)` does not compile — a context
    /// seeding site can only reach the pool by deliberately unwrapping the Arc,
    /// which is no longer the accident this module exists to stop.
    pub(super) fn repl_render_tool_pool(
        permission_context: &crate::tool::ToolPermissionContext,
        mcp_state: &crate::state::app_state_store::McpState,
        initial_tools: &[Tool],
        main_thread_agent_definition: Option<&AgentDefinition>,
    ) -> Arc<Vec<Tool>> {
        Arc::new(
            assemble_repl_prompt_tools(
                initial_tools,
                permission_context,
                mcp_state,
                main_thread_agent_definition,
            )
            .tools,
        )
    }

    /// Maps to CC `REPL.tsx:3206-3208`:
    /// `agentDefinitions: allowedAgentTypes ? { ...s.agentDefinitions,
    /// allowedAgentTypes } : s.agentDefinitions`.
    ///
    /// When the memo produced no restriction the store object passes through
    /// untouched (same identity); when it did, a shallow copy carries it for
    /// this turn only, so `AppState.agentDefinitions` never gains the field —
    /// matching `AppStateStore.ts:505`.
    fn merge_allowed_agent_types(
        store_definitions: &Arc<AgentDefinitionsResult>,
        allowed_agent_types: Option<Vec<String>>,
    ) -> Arc<AgentDefinitionsResult> {
        match allowed_agent_types {
            Some(allowed_agent_types) => {
                let mut merged = store_definitions.as_ref().clone();
                merged.allowed_agent_types = Some(allowed_agent_types);
                Arc::new(merged)
            }
            None => store_definitions.clone(),
        }
    }

    /// Maps to: CC REPL `mergedTools` + `resolveAgentTools(..., isMainThread=true)`
    /// (`REPL.tsx:1203-1221`).
    fn assemble_repl_prompt_tools(
        initial_tools: &[Tool],
        permission_context: &crate::tool::ToolPermissionContext,
        mcp_state: &crate::state::app_state_store::McpState,
        main_thread_agent_definition: Option<&AgentDefinition>,
    ) -> ReplPromptTools {
        // Maps to CC `hooks/useMergedTools.ts:30-36`:
        //   assembleToolPool(toolPermissionContext, mcpTools)
        //   mergeAndFilterTools(initialTools, assembled, mode)
        let assembled = crate::tools::assemble_tool_pool(permission_context, &mcp_state.tools);
        let prompt_tools = crate::utils::tool_pool::merge_and_filter_tools(
            initial_tools,
            assembled,
            permission_context.mode,
        );

        // Maps to CC `REPL.tsx:1205-1212`: no main-thread agent → the merged pool
        // as-is and `allowedAgentTypes: undefined`.
        let Some(agent) = main_thread_agent_definition else {
            return ReplPromptTools {
                tools: prompt_tools,
                allowed_agent_types: None,
            };
        };
        let resolved = crate::tools::agent_tool::agent_tool_utils::resolve_agent_tools(
            agent,
            &prompt_tools,
            false,
            true,
        );
        ReplPromptTools {
            tools: resolved.resolved_tools,
            allowed_agent_types: resolved.allowed_agent_types,
        }
    }
}

use repl_prompt_tools::{install_repl_prompt_tools, repl_render_tool_pool};

/// The memo dependency for [`repl_render_tool_pool`].
///
/// CC's two `useMemo`s key on the IDENTITIES of `[initialTools, mcpTools,
/// toolPermissionContext]` (`useMergedTools.ts:36-41`) and
/// `[mainThreadAgentDefinition, mergedTools]` (`REPL.tsx:1221`). iocraft's
/// `use_memo` hashes its dependency instead of comparing identity, so the same
/// four inputs are projected onto the parts the pool actually reads:
///
/// - `initial_tools` / `mcp_state.tools` — their names, in order: the pool is
///   `uniqBy(builtIn.concat(mcp), 'name')` over exactly these
///   (`tools.ts:363-366`, `utils/toolPool.ts:66`).
/// - `tool_permission_context` — the `mode` `mergeAndFilterTools` receives
///   (`useMergedTools.ts:33`) plus the deny rules `filterToolsByDenyRules`
///   applies (`tools.ts:349-352`). The allow/ask sets never reach the pool.
/// - `main_thread_agent_definition` — the frontmatter spec `resolveAgentTools`
///   parses (`agentToolUtils.ts:190-206`), keyed by agent type.
fn repl_render_tool_pool_deps(
    permission_context: &ToolPermissionContext,
    mcp_state: &crate::state::app_state_store::McpState,
    initial_tools: &[crate::types::tools::Tool],
    main_thread_agent_definition: Option<
        &crate::tools::agent_tool::load_agents_dir::AgentDefinition,
    >,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for tool in initial_tools {
        tool.name.hash(&mut hasher);
    }
    "\u{1}".hash(&mut hasher);
    for tool in &mcp_state.tools {
        tool.name.hash(&mut hasher);
    }
    std::mem::discriminant(&permission_context.mode).hash(&mut hasher);
    // `HashMap` iteration order is not stable between reads, so the rules are
    // flattened through a `BTreeSet`: an unordered walk would hash differently
    // on every render and defeat the memo outright, putting the disk-reading
    // assembly back on the frame path.
    permission_context
        .always_deny_rules
        .iter()
        .flat_map(|(source, rules)| {
            rules.iter().map(move |rule| {
                format!(
                    "{source:?}\u{1}{}\u{1}{:?}",
                    rule.tool_name, rule.rule_content
                )
            })
        })
        .collect::<std::collections::BTreeSet<_>>()
        .hash(&mut hasher);
    if let Some(agent) = main_thread_agent_definition {
        agent.agent_type.hash(&mut hasher);
        agent.tools.hash(&mut hasher);
        agent.disallowed_tools.hash(&mut hasher);
    }
    hasher.finish()
}

/// The builder-owned half of CC's `getToolUseContext`, applied to a context
/// that was NOT produced by [`build_repl_process_user_input_context`].
///
/// CC needs no such function: every prompt path receives the REPL-supplied
/// `getToolUseContext` (`handlePromptSubmit.ts:55`, `:264`, `:423`), so
/// `agentDefinitions` is always present. This port has one path that builds its
/// own — `submit_processed_prompt_deferred_query`
/// (`handle_prompt_submit.rs:110-127`) takes only a `ToolPermissionContext` and
/// calls `ToolUseContext::with_permission_context`, so `agent_definitions`
/// starts `Default` (EMPTY) and `loaded_nested_memory_paths` starts empty. Its
/// three siblings — the typed-prompt, inbox, and scheduled-queue paths — take a
/// caller-built context and are unaffected.
///
/// Called from exactly one place: [`apply_repl_query_turn_context`], the single
/// owner of the per-turn refresh. It stays a named function because the
/// regression it exists for has its own test.
///
/// Scope note: `tools` and `agent_definitions` are NOT seeded here. They are the
/// per-turn halves the owner recomputes for every path through
/// [`install_repl_prompt_tools`], so seeding them here would be a second,
/// divergable copy of `REPL.tsx:3192` + `:3206-3208`. What is left is exactly
/// the fields the builder writes once and the shell never got.
///
/// Still NOT seeded on that path, and therefore a gap rather than a contract:
/// `messages`, `response_length_sink`, `commands`, `debug`,
/// `custom_system_prompt` and `append_system_prompt`, all of which the builder
/// sets (`:524`, `:528`, `:544-545`, `:547-554`). Closing them changes
/// behaviour, so it is not part of this ownership fix.
fn seed_deferred_query_shell_context(
    context: &mut ToolUseContext,
    main_thread_agent_definition: Option<
        &crate::tools::agent_tool::load_agents_dir::AgentDefinition,
    >,
    loaded_nested_memory_paths: std::collections::HashSet<String>,
) {
    context.agent_type = main_thread_agent_definition.map(|agent| agent.agent_type.clone());
    context.loaded_nested_memory_paths = loaded_nested_memory_paths;
}

/// Maps to: CC `utils/handlePromptSubmit.ts:378-380` `makeContext()` and
/// REPL's `getToolUseContext(messages, [], abortController, mainLoopModel)`.
/// Every input source (direct, inbox, scheduled queue) must construct this
/// before slash dispatch so callbacks observe one live history/AppState owner.
#[allow(clippy::too_many_arguments)]
fn build_repl_process_user_input_context(
    tool_permission_context: ToolPermissionContext,
    app_store: &crate::state::store::AppStore,
    messages: Vec<Message>,
    resume_restore_stores: ResumeRestoreStores,
    loaded_nested_memory_paths: std::collections::HashSet<String>,
    mcp_state: crate::state::app_state_store::McpState,
    initial_tools: &[crate::types::tools::Tool],
    commands: std::sync::Arc<Vec<crate::commands::Command>>,
    debug: bool,
    system_prompt_overrides: &crate::utils::system_prompt::CliSystemPromptOverrides,
    main_thread_agent_definition: Option<
        &crate::tools::agent_tool::load_agents_dir::AgentDefinition,
    >,
    callbacks: Option<&crate::services::mcp::channel_permissions::ChannelPermissionCallbacks>,
    interactive_permission_sink: &crate::tool::InteractivePermissionSink,
    thinking_config: &crate::utils::thinking::ThinkingConfig,
    response_length_sink: crate::tool::ResponseLengthSink,
) -> ToolUseContext {
    let app_snapshot = app_store.get();
    let main_loop_model = repl_main_loop_model(&app_snapshot);
    let mut context = ToolUseContext::with_permission_context(tool_permission_context)
        .with_app_store(app_store.clone())
        .with_messages(messages)
        .with_resume_restore_stores(resume_restore_stores)
        .with_mcp_state(mcp_state)
        .with_main_loop_model(main_loop_model.clone())
        .with_response_length_sink(response_length_sink);
    // Maps to CC `REPL.tsx:3192` + `:3206-3208`. Runs AFTER `with_app_store`,
    // which is the `const s = store.getState()` half (`:3165`): the overlay is
    // the builder's LAST word on `agentDefinitions`, never the store read's.
    // `tool_permission_context` and `mcp_state` are read back off the context
    // rather than from the parameters — `with_app_store` refills the first from
    // `store.getState()` (which is where every production caller got the
    // argument: `AppStore::tool_permission_context`, `store.rs:343-345`) and
    // `.with_mcp_state` above installs the second.
    install_repl_prompt_tools(
        &mut context,
        &app_snapshot.agent_definitions,
        initial_tools,
        main_thread_agent_definition,
    );
    context.loaded_nested_memory_paths = loaded_nested_memory_paths;
    context.commands = commands;
    context.debug = debug;
    context.agent_type = main_thread_agent_definition.map(|agent| agent.agent_type.clone());
    context.custom_system_prompt = system_prompt_overrides
        .custom
        .as_deref()
        .map(str::to_string);
    context.append_system_prompt = system_prompt_overrides
        .append
        .as_deref()
        .map(str::to_string);
    apply_repl_tool_use_context_options(
        &mut context,
        callbacks,
        interactive_permission_sink,
        app_snapshot.fast_mode,
        app_snapshot.thinking_enabled,
        thinking_config,
        Some(main_loop_model),
        app_snapshot.effort_value.clone(),
    );
    context
}

/// Maps to CC `REPL.tsx`'s `hasInterruptibleToolInProgress` ref setter.  The
/// query actor clones `ToolUseContext`, so an `Arc<AtomicBool>` gives the
/// synchronous prompt submit callback the same live value without routing a
/// second UI event through the render loop.
fn install_repl_interruptible_tool_sink(
    context: &mut ToolUseContext,
    state: &Arc<std::sync::atomic::AtomicBool>,
) {
    let state = Arc::clone(state);
    context.set_has_interruptible_tool_in_progress =
        crate::tool::SetHasInterruptibleToolInProgress(Some(Arc::new(move |value| {
            state.store(value, std::sync::atomic::Ordering::SeqCst);
        })));
}

/// The per-turn refresh every REPL deferred-query path applies to the context
/// `handle_prompt_submit` handed back, plus the `QueryParams` fields derived
/// from it. This is the ONE owner: a field belonging to the turn contract is
/// written here, or it reaches no path at all.
///
/// Maps to: CC `screens/REPL.tsx:3674-3679`. CC never patches the context that
/// ran the dispatch — `onQuery` calls the REPL-supplied builder
/// `getToolUseContext(messagesIncludingNewMessages, newMessages,
/// abortController, mainLoopModel)` a SECOND time, fresh, so `computeTools()`
/// and `store.getState()` (`:3165`, `:3172-3186`) observe whatever the dispatch
/// changed — notably the command-scoped `alwaysAllowRules.command` write
/// immediately above it (`:3634-3653`, this port's
/// `apply_repl_command_allowed_tools`). `:3699-3750` then derives
/// `systemPrompt` / `userContext` / `systemContext` from that fresh context,
/// and `:3757-3765` hands both to `query(...)`.
///
/// The port refreshes the returned context rather than rebuilding it, because
/// the dispatch result carries edits the builder has no input for: `/plan`
/// installs its prepared permission mode on the way out
/// (`handle_prompt_submit.rs:240-244`; on the three builder-fed paths the
/// context carries the app store, so `update_permission_context` writes through
/// and the `command_permission_context` below reads it back), and the
/// content-replacement state is provisioned and written back here rather than
/// recomputed by the builder.
///
/// Refreshing instead of rebuilding means CC's second builder call has to be
/// reassembled out of two pieces, and BOTH are mandatory: `attach_app_store` is
/// the `store.getState()` half (`:3165`) and [`install_repl_prompt_tools`] is
/// the memo half (`:3192` + `:3206-3208`). Porting only the first is what let
/// the `Agent(x,y)` restriction go missing a third time — a store re-read
/// cannot restore a value AppState never holds.
///
/// Returns the content-replacement state the caller must publish back into REPL
/// state (`None` when the feature is off), mirroring CC's
/// `contentReplacementStateRef` being shared by identity rather than by value.
#[allow(clippy::too_many_arguments)]
#[must_use]
fn apply_repl_query_turn_context(
    params: &mut crate::query::QueryParams,
    app_store: &crate::state::store::AppStore,
    command_permission_context: ToolPermissionContext,
    history_entries: &[HistoryEntry],
    content_replacement_state: crate::utils::tool_result_storage::ContentReplacementState,
    resume_restore_stores: &ResumeRestoreStores,
    mcp_state: Option<crate::state::app_state_store::McpState>,
    main_thread_agent_definition: Option<
        &crate::tools::agent_tool::load_agents_dir::AgentDefinition,
    >,
    initial_tools: &[crate::types::tools::Tool],
    system_prompt_overrides: &crate::utils::system_prompt::CliSystemPromptOverrides,
    channel_permission_callbacks: Option<
        &crate::services::mcp::channel_permissions::ChannelPermissionCallbacks,
    >,
    interactive_permission_sink: &crate::tool::InteractivePermissionSink,
    has_interruptible_tool_in_progress: &Arc<std::sync::atomic::AtomicBool>,
    thinking_config: &crate::utils::thinking::ThinkingConfig,
    shell_context_seed: Option<std::collections::HashSet<String>>,
) -> Option<crate::utils::tool_result_storage::ContentReplacementState> {
    params.tool_use_context.tool_permission_context = command_permission_context;

    // C3c-4: the submitted user rows already entered the ONE history as
    // `Message` entries; both query views below are projections of that one
    // history, so the caller must append before calling in.
    params.messages = project_history_rows(history_entries);
    let typed_history = history_model_messages(history_entries);
    params.model_messages = typed_history.clone();

    let should_reconstruct_content_replacement_state = resume_restore_stores.session_id.is_some()
        && content_replacement_state
            == crate::utils::tool_result_storage::ContentReplacementState::new();
    let query_content_replacement_state = if should_reconstruct_content_replacement_state {
        crate::utils::tool_result_storage::provision_content_replacement_state(
            Some(&typed_history),
            &resume_restore_stores.content_replacements,
        )
    } else if crate::utils::tool_result_storage::is_content_replacement_enabled() {
        Some(content_replacement_state)
    } else {
        None
    };
    params.tool_use_context.content_replacement_state = query_content_replacement_state.clone();

    // Maps to CC `const mainLoopModel = useMainLoopModel()`.
    let app_snapshot = app_store.get();
    let prompt_model = repl_main_loop_model(&app_snapshot);
    params.attach_app_store(app_store.clone());
    if let Some(mcp_state) = mcp_state {
        params.tool_use_context.mcp_state = mcp_state;
    }
    // The MCP-prompt path's context never met the builder, so give it the
    // builder-once fields first; everything below is per-turn and shared.
    if let Some(loaded_nested_memory_paths) = shell_context_seed {
        seed_deferred_query_shell_context(
            &mut params.tool_use_context,
            main_thread_agent_definition,
            loaded_nested_memory_paths,
        );
    }
    // Maps to CC `REPL.tsx:3192` + `:3206-3208`, the two lines the SECOND
    // `getToolUseContext` call at `:3674-3679` re-runs.
    //
    // `attach_app_store` above is only half of that second call — the
    // `store.getState()` half (`:3165`). CC's builder then puts the memo's
    // per-turn overlay on top (`:3206-3208`), and that overlay is the one thing
    // AppState deliberately never holds (`AppStateStore.ts:505` has no
    // `allowedAgentTypes`), so a store re-read cannot restore it. Everything
    // else `with_app_store` overwrites IS store-backed in CC and correctly
    // re-read: `tool_permission_context` (`:3175`, written to the store by
    // `apply_repl_command_allowed_tools` before this call, exactly as CC writes
    // `alwaysAllowRules.command` at `:3634-3653` before `:3674`), `mcp_state`
    // (`:3200-3201`), `verbose` (`:3194`), `channel_permission_callbacks`.
    // `agent_definitions` was the only overlay-bearing field being re-read
    // without being re-applied — this call is the re-apply.
    install_repl_prompt_tools(
        &mut params.tool_use_context,
        &app_snapshot.agent_definitions,
        initial_tools,
        main_thread_agent_definition,
    );
    // Maps to CC `REPL.tsx:3715-3722` `getSystemPrompt(freshTools,
    // mainLoopModelParam, Array.from(toolPermissionContext
    // .additionalWorkingDirectories.keys()), freshMcpClients)`.
    let additional_working_directories: Vec<String> = params
        .tool_use_context
        .tool_permission_context
        .additional_working_directories
        .keys()
        .cloned()
        .collect();
    let (system_prompt, user_context, system_context) = build_repl_query_prompt_contexts(
        system_prompt_overrides,
        crate::constants::prompts::get_system_prompt(
            &params.tool_use_context.tools,
            &prompt_model,
            &additional_working_directories,
            &params.tool_use_context.mcp_state.clients,
        ),
        main_thread_agent_definition,
        &params.tool_use_context.mcp_state,
    );
    // Maps to: CC `REPL.tsx:3750` `toolUseContext.renderedSystemPrompt =
    // systemPrompt` — the parent's rendered prompt bytes are frozen at turn
    // start so fork spawn/resume can share the parent's prompt cache
    // (`Tool.ts:293-299`). CC's other write site, `REPL.tsx:3371`
    // (`handleBackgroundQuery`, Ctrl+B session backgrounding), belongs to a
    // flow this port has not built; when it lands it must write this field the
    // same way.
    params.tool_use_context.rendered_system_prompt = Some(system_prompt.clone());
    params.system_prompt = system_prompt;
    params.user_context = user_context;
    params.system_context = system_context;
    params.tool_use_context.resume_restore_stores = resume_restore_stores.clone();

    apply_repl_tool_use_context_options(
        &mut params.tool_use_context,
        channel_permission_callbacks,
        interactive_permission_sink,
        app_snapshot.fast_mode,
        app_snapshot.thinking_enabled,
        thinking_config,
        Some(prompt_model),
        app_snapshot.effort_value.clone(),
    );
    // Maps to CC `REPL.tsx`'s per-turn `hasInterruptibleToolInProgress` ref
    // installation. Keeping this inside the single context owner prevents
    // MCP, typed-prompt, inbox, and scheduled-query paths from diverging.
    install_repl_interruptible_tool_sink(
        &mut params.tool_use_context,
        has_interruptible_tool_in_progress,
    );

    query_content_replacement_state
}

impl ReplStartupDialogSnapshot {
    /// Maps to CC `screens/REPL.tsx` startup dialog seed construction for
    /// `shouldShowDesktopUpsellStartup()` using already-loaded config.
    ///
    /// Remote Callout remains an explicit seam until bridge/auth runtime state
    /// is ported: guessing the official `feature('BRIDGE_MODE')`, OAuth-token,
    /// and bridge-entitlement chain would surface an action Cometix cannot yet
    /// safely execute.
    pub fn from_readonly_global_config(global_config: &GlobalConfig) -> Self {
        Self {
            remote_callout: None,
            desktop_upsell: Some(DesktopUpsellStartupSnapshot {
                platform: node_platform_from_rust(),
                arch: node_arch_from_rust(),
                config: get_desktop_upsell_config(),
                desktop_upsell_dismissed: global_config.desktop_upsell_dismissed.unwrap_or(false),
                desktop_upsell_seen_count: global_config.desktop_upsell_seen_count.unwrap_or(0),
            }),
            desktop_handoff_state: DesktopHandoffState::Checking,
            desktop_handoff_error: None,
            desktop_handoff_download_message: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplStartupDialogKind {
    RemoteCallout,
    PluginHint,
    DesktopUpsell,
}

#[derive(Debug, Clone, PartialEq)]
enum LocalCommandPanel {
    PluginSettings {
        data: crate::commands::plugin::plugin::PluginSettingsData,
        preceding_input_blocks: Vec<UserContent>,
        /// Actual REPL immediate execution, not the display-slot classification.
        immediate_execution: bool,
    },
    AddDir {
        args: String,
    },
    Tasks {
        context: Arc<crate::tool::ToolUseContext>,
    },
    Settings(SettingsTab),
    Resume,
    Help,
    Hooks,
    Model,
    Fast,
    Theme,
    Export(
        crate::commands::export::export::ExportDialogData,
        Vec<crate::types::message::UserContent>,
    ),
    Permissions,
    Memory,
    Agents,
    Skills,
    Stats,
    Doctor,
    Diff,
    Sandbox,
    Btw {
        question: String,
        context: Arc<crate::tool::ToolUseContext>,
    },
    Copy(crate::commands::copy::CopyPickerData),
    Mcp {
        args: String,
    },
    Login {
        args: String,
    },
    Logout {
        args: String,
    },
    Ide {
        args: String,
    },
    Effort {
        args: Option<String>,
        has_conversation_messages: bool,
    },
    It2Setup {
        request: It2SetupPromptRequest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalCommandInvocation {
    slash_command: SlashCommandInvocation,
    dismiss_result: Option<String>,
}

/// Rust-side state for official CC `toolJSX` / local-jsx command UI.
#[derive(Debug, Clone, PartialEq)]
struct ActiveLocalCommandUi {
    // L1 Promise first resolve, shared by the owned clipboard callbacks of one
    // invocation. Maps to processSlashCommand.tsx:733 local-jsx Promise.
    completion: Arc<std::sync::OnceLock<()>>,
    panel: LocalCommandPanel,
    invocation: Option<LocalCommandInvocation>,
    should_hide_prompt_input: bool,
    is_local_command_ui: bool,
    is_immediate: bool,
}

#[derive(Clone)]
struct PendingRuntimeMessage {
    message: RenderableMessage,
    due_at: Instant,
    classifier_checking: Option<ClassifierChecking>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StreamingTextPreview {
    /// Raw accumulated text_delta content. Maps to CC `REPL.tsx` `streamingText`.
    raw: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamingTextDisplayMode {
    /// Strict CC parity: render only completed lines from `streamingText`.
    Line,
    /// Cometix extension: render the unstable suffix too, preserving the
    /// accidental character-by-character streaming effect without mutating the
    /// formal transcript message list.
    Character,
}

// Code-level switch for the streaming preview presentation. The state model
// stays aligned with CC either way: deltas update `StreamingTextPreview`, and
// only completed assistant blocks enter transcript history.
const STREAMING_TEXT_DISPLAY_MODE: StreamingTextDisplayMode = StreamingTextDisplayMode::Character;

/// Maps to: CC REPL.tsx:1403 `PROMPT_SUPPRESSION_MS = 1500` — how long after
/// the last keystroke interrupt dialogs stay suppressed.
const PROMPT_SUPPRESSION_MS: u64 = 1500;

fn visible_streaming_text(raw: &str, mode: StreamingTextDisplayMode) -> Option<String> {
    match mode {
        StreamingTextDisplayMode::Line => {
            let idx = raw.rfind('\n')?;
            let visible = &raw[..=idx];
            (!visible.is_empty()).then(|| visible.to_string())
        }
        StreamingTextDisplayMode::Character => (!raw.is_empty()).then(|| raw.to_string()),
    }
}

/// Inputs to CC `REPL.tsx`'s `showSpinner` predicate. Keeping this pure makes
/// every suppression/source branch testable without creating another state
/// owner; REPL still owns all live inputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ShowSpinnerInput {
    tool_jsx_allows_spinner: bool,
    tool_use_confirm_queue_empty: bool,
    prompt_queue_empty: bool,
    is_loading: bool,
    user_input_on_processing: bool,
    has_running_teammates: bool,
    command_queue_len: usize,
    pending_worker_request: bool,
    only_sleep_tool_active: bool,
    visible_streaming_text: bool,
    is_brief_only: bool,
}

/// Maps to: CC `screens/REPL.tsx#showSpinner`.
fn should_show_spinner(input: ShowSpinnerInput) -> bool {
    input.tool_jsx_allows_spinner
        && input.tool_use_confirm_queue_empty
        && input.prompt_queue_empty
        && (input.is_loading
            || input.user_input_on_processing
            || input.has_running_teammates
            || input.command_queue_len > 0)
        && !input.pending_worker_request
        && !input.only_sleep_tool_active
        && (!input.visible_streaming_text || input.is_brief_only)
}

/// Maps to: CC `REPL.tsx:2223` — the "is every live tool a Sleep?" check reads
/// `inProgressToolUseIDs` (REPL-owned state) and the last typed assistant
/// message, which stays the authority for tool names and grouping.
///
/// This used to reconstruct the set by scanning retained ToolUse rows for a
/// `Queued | Running` status, which inverted CC's authority direction: CC
/// derives a row's status *from* the set (`AssistantToolUseMessage.tsx:120-121`),
/// so deriving the set from row statuses made the rows load-bearing state and
/// forced the actor to re-emit a whole row on every status transition.
fn only_sleep_tool_active(
    model_messages: &[Message],
    in_progress_ids: &std::collections::HashSet<String>,
) -> bool {
    if in_progress_ids.is_empty() {
        return false;
    }

    let Some(last_assistant) = model_messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(message) => Some(message),
            _ => None,
        })
    else {
        return false;
    };
    let active = last_assistant
        .content
        .iter()
        .filter_map(|block| match block {
            crate::types::message::AssistantContent::ToolUse(tool_use)
                if in_progress_ids.contains(&tool_use.id.0) =>
            {
                Some(tool_use)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    !active.is_empty()
        && active
            .iter()
            .all(|tool_use| tool_use.name == crate::tools::sleep_tool::prompt::SLEEP_TOOL_NAME)
}

fn stream_api_event_value(event: &crate::types::message::StreamEvent) -> &serde_json::Value {
    match event {
        crate::types::message::StreamEvent::ApiEvent { event, .. } => event,
    }
}

fn stream_event_text_delta_text(event: &crate::types::message::StreamEvent) -> Option<&str> {
    let value = stream_api_event_value(event);
    (value.get("type")?.as_str()? == "content_block_delta").then_some(())?;
    let delta = value.get("delta")?;
    (delta.get("type")?.as_str()? == "text_delta").then_some(())?;
    delta.get("text")?.as_str()
}

fn stream_event_text_delta(event: &crate::types::message::StreamEvent) -> Option<String> {
    stream_event_text_delta_text(event).map(str::to_string)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StopHookSpinnerState {
    tool_use_id: Option<String>,
    hook_event: Option<String>,
    total: usize,
    completed: usize,
    status_message: Option<String>,
}

impl StopHookSpinnerState {
    fn apply(&mut self, event: StopHookProgressEvent) {
        match event {
            StopHookProgressEvent::Started {
                tool_use_id,
                hook_event,
                status_message,
                total,
                ..
            } => {
                if self.tool_use_id.as_deref() != Some(tool_use_id.as_str()) {
                    *self = Self {
                        tool_use_id: Some(tool_use_id),
                        hook_event: Some(hook_event),
                        total,
                        completed: 0,
                        status_message,
                    };
                    return;
                }
                self.total = self.total.max(total);
                if self.hook_event.is_none() {
                    self.hook_event = Some(hook_event);
                }
                if self.status_message.is_none() {
                    self.status_message = status_message;
                }
            }
            StopHookProgressEvent::Completed {
                tool_use_id,
                hook_event,
            } => {
                if self.tool_use_id.as_deref() == Some(tool_use_id.as_str()) {
                    if self.hook_event.is_none() {
                        self.hook_event = Some(hook_event);
                    }
                    self.completed = self.completed.saturating_add(1).min(self.total.max(1));
                }
            }
            StopHookProgressEvent::Finished { tool_use_id } => {
                if self.tool_use_id.as_deref() == Some(tool_use_id.as_str()) {
                    *self = Self::default();
                }
            }
        }
    }
}

fn stop_hook_spinner_suffix(state: &StopHookSpinnerState, is_loading: bool) -> Option<String> {
    if !is_loading || state.tool_use_id.is_none() || state.total == 0 {
        return None;
    }

    if let Some(status_message) = state
        .status_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        return Some(if state.total == 1 {
            format!("{status_message}…")
        } else {
            format!("{status_message}… {}/{}", state.completed, state.total)
        });
    }

    let hook_type = if state.hook_event.as_deref() == Some("SubagentStop") {
        "subagent stop"
    } else {
        "stop"
    };
    Some(if state.total == 1 {
        format!("running {hook_type} hook")
    } else {
        format!("running stop hooks… {}/{}", state.completed, state.total)
    })
}

fn stream_event_type(event: &crate::types::message::StreamEvent) -> Option<&str> {
    stream_api_event_value(event)
        .get("type")
        .and_then(serde_json::Value::as_str)
}

fn stream_event_content_block_start_type(
    event: &crate::types::message::StreamEvent,
) -> Option<&str> {
    (stream_event_type(event)? == "content_block_start").then_some(())?;
    stream_api_event_value(event)
        .get("content_block")?
        .get("type")?
        .as_str()
}

/// Maps to: CC `screens/REPL.tsx` stream content-block start handling.
fn stream_event_is_content_block_start(event: &crate::types::message::StreamEvent) -> bool {
    stream_event_type(event) == Some("content_block_start")
}

fn stream_event_delta_for_response_length(
    event: &crate::types::message::StreamEvent,
) -> Option<&str> {
    (stream_event_type(event)? == "content_block_delta").then_some(())?;
    let delta = stream_api_event_value(event).get("delta")?;
    match delta.get("type")?.as_str()? {
        "text_delta" => delta.get("text")?.as_str(),
        "input_json_delta" => delta.get("partial_json")?.as_str(),
        "thinking_delta" => delta.get("thinking")?.as_str(),
        // Maps to CC `handleMessageFromStream`: signatures are authentication
        // bytes, not visible output, so they must not inflate token estimates.
        "signature_delta" => None,
        _ => None,
    }
}

fn stream_block_type_to_spinner_mode(block_type: &str) -> SpinnerMode {
    match block_type {
        "thinking" | "redacted_thinking" => SpinnerMode::Thinking,
        "text" => SpinnerMode::Responding,
        "tool_use"
        | "server_tool_use"
        | "web_search_tool_result"
        | "code_execution_tool_result"
        | "mcp_tool_use"
        | "mcp_tool_result"
        | "container_upload"
        | "web_fetch_tool_result"
        | "bash_code_execution_tool_result"
        | "text_editor_code_execution_tool_result"
        | "tool_search_tool_result"
        | "compaction" => SpinnerMode::ToolInput,
        _ => SpinnerMode::Responding,
    }
}

/// Maps to: CC `REPL.tsx:2835-2844` — `if (streamingText?.trim())
/// setMessages(prev => [...prev, createAssistantMessage({ content:
/// streamingText })])`: the partially-streamed text is promoted into the ONE
/// messages array as a whole synthetic assistant, so it is rendered, recorded
/// to the transcript (useLogMessages walks the same array), and model-visible
/// on the next turn — order [.. streamed blocks, partial-assistant,
/// "[Request interrupted by user]"]. Completed blocks are already in history
/// (per-block `QueryEvent::Message` appends); only never-block-stopped text
/// lives in the preview, so nothing doubles.
fn append_partial_streaming_text_message(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    preview: Option<StreamingTextPreview>,
) {
    let Some(preview) = preview else {
        return;
    };
    if preview.raw.trim().is_empty() {
        return;
    }
    let message = Message::Assistant(crate::utils::messages::create_assistant_message(
        preview.raw,
    ));
    let mut entries = history_state.read().as_ref().clone();
    entries.push(HistoryEntry::Message(message));
    history_state.set(Arc::new(entries));
}

#[derive(Default)]
struct QueryPumpBurstProfile {
    started_at: Option<Instant>,
    events: usize,
    stream_events: usize,
    text_delta_events: usize,
    text_delta_bytes: usize,
    content_block_starts: usize,
    renderable_messages: usize,
    model_messages: usize,
    tool_progress: usize,
    stop_hook_progress: usize,
    permission_requests: usize,
    terminal_events: usize,
    clear_preview_events: usize,
    other_events: usize,
}

impl QueryPumpBurstProfile {
    fn record_event(&mut self, event: &QueryEvent) {
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }
        self.events += 1;
        match event {
            QueryEvent::Stream(stream_event) => {
                self.stream_events += 1;
                if let Some(text) = stream_event_text_delta_text(stream_event) {
                    self.text_delta_events += 1;
                    self.text_delta_bytes += text.len();
                } else if stream_event_is_content_block_start(stream_event) {
                    self.content_block_starts += 1;
                }
            }
            QueryEvent::Row(_) => self.renderable_messages += 1,
            QueryEvent::Message(_) | QueryEvent::ModelMessage(_) => self.model_messages += 1,
            QueryEvent::ToolProgress(_) => self.tool_progress += 1,
            QueryEvent::StopHookProgress(_) => self.stop_hook_progress += 1,
            QueryEvent::PermissionRequest(_) => self.permission_requests += 1,
            QueryEvent::Terminal(_) => self.terminal_events += 1,
            QueryEvent::ClearStreamingPreview => self.clear_preview_events += 1,
            _ => self.other_events += 1,
        }
    }

    fn maybe_log_and_reset(&mut self, queue_len: usize, queue_empty: bool) {
        let Some(started_at) = self.started_at else {
            return;
        };
        let elapsed = started_at.elapsed();
        let should_log = (queue_empty && (self.events > 1 || elapsed >= Duration::from_millis(16)))
            || self.events >= 128
            || elapsed >= Duration::from_millis(250);
        if !should_log {
            return;
        }
        eprintln!(
            "cometix-query-pump burst elapsed={:?} events={} stream={} text_delta={} text_bytes={} content_block_start={} renderable_message={} model_message={} tool_progress={} stop_hook_progress={} permission_request={} clear_preview={} terminal={} other={} queue_len={} complete={}",
            elapsed,
            self.events,
            self.stream_events,
            self.text_delta_events,
            self.text_delta_bytes,
            self.content_block_starts,
            self.renderable_messages,
            self.model_messages,
            self.tool_progress,
            self.stop_hook_progress,
            self.permission_requests,
            self.clear_preview_events,
            self.terminal_events,
            self.other_events,
            queue_len,
            queue_empty,
        );
        *self = Self::default();
    }
}

fn current_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Clone, Debug, PartialEq)]
struct SpeculationAcceptanceOutcome {
    input: String,
    clean_messages: Vec<Message>,
    result: Option<crate::services::prompt_suggestion::speculation::SpeculationResult>,
    active: crate::state::app_state_store::ActiveSpeculationState,
}

#[derive(Clone, Debug, PartialEq)]
struct McpPromptSlashCommandSubmit {
    input: String,
    turn_id: String,
    command: McpPromptCommandSnapshot,
    server_name: String,
    prompt: McpPromptSnapshot,
    args: String,
    tool_permission_context: ToolPermissionContext,
}

/// What the synchronous `on_submit` callback hands to the async submit path.
///
/// CC's `onSubmit` is itself `async` (`REPL.tsx:4241-4242`), so upstream simply
/// awaits `handlePromptSubmit` inline. iocraft input handlers are synchronous
/// (`HandlerMut<PromptSubmission>`), and `handlePromptSubmit` must await
/// `processUserInput` — which runs the `UserPromptSubmit` hooks, and those can
/// erase the submission or stop the turn. So the callback resolves its own
/// early-return branches (bash mode, speculation accept, MCP prompt commands)
/// and passes the rest here, the same shape the MCP prompt path already uses.
#[derive(Clone)]
struct PromptQuerySubmit {
    text: String,
    image_contents: Vec<crate::types::message::UserContent>,
    image_paste_ids: Vec<u32>,
    from_keybinding: bool,
    /// CC `REPL.tsx:4429` vs `:4525`: opening a local command UI while a query
    /// runs is the immediate local-jsx path, which must not clear the selection.
    is_query_active_for_immediate: bool,
    tool_permission_context: ToolPermissionContext,
    runtime_mcp_state: crate::state::app_state_store::McpState,
    /// Awaited local-JSX completion rejoins the same submit/query tail.
    completed_local_command: Option<CompletedLocalCommand>,
}

/// Native capture of CC processUserInput's suspended local-JSX invocation.
#[derive(Clone)]
struct LocalCommandContinuation {
    preceding_input_blocks: Vec<UserContent>,
    input: String,
    context: ToolUseContext,
    hook_session_id: String,
}

#[derive(Clone)]
struct CompletedLocalCommand {
    continuation: LocalCommandContinuation,
    processed: crate::utils::process_user_input::ProcessUserInputBaseResult,
}

fn schedule_pending_query_messages(
    pending_responses: &mut State<Vec<PendingRuntimeMessage>>,
    pending_messages: Vec<PendingQueryMessage>,
) {
    let now = Instant::now();
    let mut pending = pending_responses.read().clone();
    pending.extend(
        pending_messages
            .into_iter()
            .map(|pending| PendingRuntimeMessage {
                message: pending.message,
                due_at: now + pending.delay,
                classifier_checking: pending.classifier_checking,
            }),
    );
    pending_responses.set(pending);
}

/// Rust/iocraft projection of CC
/// `screens/REPL.tsx:3622-3649#onQueryImpl` command-scoped `allowedTools`
/// update. Rust has multiple actor ingress paths, so they share this exact REPL
/// state owner before the `shouldQuery` gate.
fn apply_repl_command_allowed_tools(
    store: &crate::state::store::AppStore,
    allowed_tools: &[String],
) -> ToolPermissionContext {
    let rules = allowed_tools
        .iter()
        .map(|rule| {
            crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string(
                rule,
            )
        })
        .collect::<Vec<_>>();
    let mut context = store.tool_permission_context();
    if context
        .always_allow_rules
        .get(&crate::types::permissions::PermissionRuleSource::Command)
        != Some(&rules)
    {
        context.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Command,
            rules,
        );
        store.set_tool_permission_context(context.clone());
    }
    context
}

/// Converts a REPL-produced transcript row into its single-history entry.
///
/// Maps to: CC's model-visibility-by-type rule — user rows carry the
/// whole wire `UserMessage` and enter the one messages array
/// (`REPL.tsx:3901` `setMessages(old => [...old, ...newMessages])`). Attachments
/// returned by processUserInput enter that same typed history; their API/log
/// visibility is decided by the canonical normalizer and audience filter.
/// System local-command rows also enter that typed history: CC
/// `processSlashCommand.tsx:780-791` returns `createCommandInputMessage`
/// (`utils/messages.ts:4516-4526`), which is a persistent system message.
/// Its conversion to API user content belongs to normalizeMessagesForAPI
/// (`utils/messages.ts:2059-2093`), not this history/persistence boundary. Other unmatched row kinds retain their
/// existing render-only transport. The user replay guard rejects tool_result
/// blocks without an id.
fn history_entry_from_row(row: RenderableMessage) -> HistoryEntry {
    match &row.kind {
        RenderableMessageKind::User { message, .. } => {
            let replayable = !message.content.iter().any(|block| {
                matches!(
                    block,
                    crate::types::message::UserContent::ToolResult(tool_result)
                        if tool_result.tool_use_id.0.is_empty()
                )
            });
            if replayable {
                HistoryEntry::Message(Message::User(message.clone()))
            } else {
                HistoryEntry::Row(row)
            }
        }
        RenderableMessageKind::System(message @ SystemMessage::LocalCommand { .. }) => {
            let mut message = message.clone();
            // The row UUID is the identity carried by the command result.
            // Unify the nested typed envelope before logging/render projection.
            message.base_mut().uuid = row.uuid.clone();
            HistoryEntry::Message(Message::System(message))
        }
        RenderableMessageKind::Attachment(attachment) => HistoryEntry::Message(
            Message::Attachment(crate::types::message::AttachmentMessage {
                uuid: row.uuid.clone(),
                timestamp: chrono::Utc::now(),
                attachment: attachment.clone(),
                wire_payload: None,
            }),
        ),
        _ => HistoryEntry::Row(row),
    }
}

fn append_history_rows(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    rows: impl IntoIterator<Item = RenderableMessage>,
) {
    // Owned async local-jsx callbacks can finish after REPL unmount. Like
    // React setState, the terminal State setter then becomes a safe no-op.
    let Some(mut history) = history_state.try_write() else {
        return;
    };
    let mut entries = history.as_ref().clone();
    entries.extend(rows.into_iter().map(history_entry_from_row));
    *history = Arc::new(entries);
}

fn append_prompt_submit_result(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    pending_responses: &mut State<Vec<PendingRuntimeMessage>>,
    result: HandlePromptSubmitResult,
) {
    append_history_rows(history_state, result.messages);
    schedule_pending_query_messages(pending_responses, result.pending);
}

/// Maps to: CC `REPL.tsx:3496` `setMessages(oldMessages => [...oldMessages,
/// newMessage])` — CC's stream handler appends whole messages. The
/// replace-by-uuid half is the Rust seam's re-yield contract: an actor that
/// re-sends the same-uuid message/row updates the entry in place instead of
/// duplicating it (CC never re-yields; its special-case replacements are the
/// ephemeral-progress arm `REPL.tsx:3482-3496` and the tombstone filter).
fn push_or_replace_entry(entries: &mut Vec<HistoryEntry>, entry: HistoryEntry) {
    if let Some(existing) = entries
        .iter_mut()
        .find(|existing| existing.uuid() == entry.uuid())
    {
        *existing = entry;
    } else {
        entries.push(entry);
    }
}

/// Maps to CC `REPL.tsx:3522-3525` tombstone consumer: `setMessages(old =>
/// old.filter(m => m !== tombstonedMessage))` — the query yields a tombstone
/// for an orphaned partial assistant message and REPL removes it. Rust keys
/// on uuid because the tombstoned value crossed a channel.
fn remove_tombstoned_entry(entries: &mut Vec<HistoryEntry>, uuid: &str) {
    entries.retain(|entry| entry.uuid() != uuid);
}

/// Maps to: CC `REPL.tsx:4904-4920` `rewindConversationTo` —
/// `setMessages(prev.slice(0, messageIndex))`: truncate the ONE history to
/// just before the selected message. The selector hands back a projected row
/// uuid, so the cut point is the first entry whose projected rows contain it
/// (replaying the normalizer walk keeps derived split-row ids addressable).
fn truncate_history_before_row(
    entries: &[HistoryEntry],
    row_uuid: &str,
) -> Option<Vec<HistoryEntry>> {
    let mut normalizer = crate::utils::messages::MessageNormalizer::default();
    let mut scratch = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.uuid() == row_uuid {
            return Some(entries[..index].to_vec());
        }
        scratch.clear();
        match entry {
            HistoryEntry::Message(message) => normalizer.push(message, &mut scratch),
            HistoryEntry::Row(row) => scratch.push(row.clone()),
            HistoryEntry::ModelOnly(_) => {}
        }
        if scratch.iter().any(|row| row.uuid == row_uuid) {
            return Some(entries[..index].to_vec());
        }
    }
    None
}

/// Maps to: CC `screens/REPL.tsx:4904-4958` `rewindConversationTo`.
/// L1: the selector's normalized row uuid identifies the original history
/// entry; input restoration always uses that entry, never its display title.
fn rewind_conversation_to(
    mut history: State<Arc<Vec<HistoryEntry>>>,
    mut conversation_id: State<u64>,
    store: &crate::state::store::AppStore,
    row_uuid: &str,
) -> Option<crate::types::message::UserMessage> {
    let entries = Arc::clone(&history.read());
    let truncated = truncate_history_before_row(&entries, row_uuid)?;
    let message = match &entries[truncated.len()] {
        HistoryEntry::Message(Message::User(message)) => message.clone(),
        HistoryEntry::Row(RenderableMessage {
            kind: RenderableMessageKind::User { message, .. },
            ..
        }) => message.clone(),
        _ => return None,
    };
    history.set(Arc::new(truncated));
    conversation_id.set(conversation_id.get().wrapping_add(1));
    crate::services::compact::micro_compact::reset_microcompact_state();
    store.set_state(|prev| {
        let mut next = (**prev).clone();
        if let Some(mode) = message.permission_mode.as_deref() {
            Arc::make_mut(&mut next.tool_permission_context).mode =
                crate::utils::permissions::permission_mode::permission_mode_from_string(mode);
        }
        next.prompt_suggestion = Default::default();
        crate::state::store::UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
    Some(message)
}

/// Maps to: CC `screens/REPL.tsx:4964-4999` `restoreMessageSync`, input half.
/// L1 retained state carrier: optional mode/images represent the independent
/// CC setters; an image-free message does not clear existing pasted contents.
fn restore_message_sync(
    message: &crate::types::message::UserMessage,
    revision: u64,
    current_input: &str,
) -> crate::components::prompt_input::PromptInputTextUpdate {
    use crate::components::prompt_input::input_paste::PastedContent;
    let resubmit = crate::utils::messages::text_for_resubmit(message);
    let image_blocks = message
        .content
        .iter()
        .filter(|block| {
            matches!(
                block,
                crate::types::message::UserContent::Image { .. }
                    | crate::types::message::UserContent::MetaImage { .. }
                    | crate::types::message::UserContent::RawImage { .. }
            )
        })
        .collect::<Vec<_>>();
    let images = image_blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            let (media_type, data) = match block {
                crate::types::message::UserContent::Image { media_type, data }
                | crate::types::message::UserContent::MetaImage { media_type, data } => {
                    (media_type.as_str(), data.as_str())
                }
                crate::types::message::UserContent::RawImage { block, .. }
                    if block
                        .pointer("/source/type")
                        .and_then(serde_json::Value::as_str)
                        == Some("base64") =>
                {
                    (
                        block.pointer("/source/media_type")?.as_str()?,
                        block.pointer("/source/data")?.as_str()?,
                    )
                }
                _ => return None,
            };
            let id = message
                .image_paste_ids
                .as_ref()
                .and_then(|ids| ids.get(index))
                .map_or(index + 1, |id| *id as usize);
            Some((
                id,
                PastedContent::Image {
                    id,
                    media_type: Some(media_type.to_string()),
                    data: Some(data.to_string()),
                    filename: None,
                    dimensions: None,
                    source_path: None,
                },
            ))
        })
        .collect::<BTreeMap<_, _>>();
    crate::components::prompt_input::PromptInputTextUpdate {
        revision,
        text: resubmit
            .as_ref()
            .map_or_else(|| current_input.to_string(), |(text, _)| text.clone()),
        mode: resubmit.map(|(_, mode)| mode),
        pasted_contents: (!image_blocks.is_empty()).then_some(images),
        cursor_offset: None,
    }
}

/// Maps to CC `runToolUse(...)` forwarding `ProgressMessage<AgentToolProgress>`
/// to the matching in-flight AssistantToolUseMessage row: the actor carries the
/// model `Message`, the render carrier CC's `NormalizedMessage`.
///
/// The producer already ran `normalizeMessages([message])` (AgentTool.tsx:1493),
/// so this is the identity re-wrap through the one normalize owner rather than
/// a second split. `None` only when the message projects to no row at all.
///
/// `pub(crate)` so the Agent tool's producer-side chain test can cross this
/// seam with the production converter instead of re-deriving it.
pub(crate) fn subagent_progress_render_message(
    message: crate::types::message::Message,
) -> Option<crate::types::message::RenderableMessage> {
    crate::utils::messages::normalize_messages(std::slice::from_ref(&message))
        .into_iter()
        .next()
}

/// Maps to: CC `services/tools/toolExecution.ts:550`
/// `stream.enqueue({ message: createProgressMessage({ toolUseID,
/// parentToolUseID, data }) })` — progress is an independent message appended
/// to the array, not a mutation of the tool-use row. `buildMessageLookups`
/// groups it by `parentToolUseID` and the renderer reads the lookups
/// (`MessageRow.tsx:154-155`); the render list drops the message itself
/// (`Messages.tsx:590`).
///
/// Append-or-replace mirrors CC `REPL.tsx:3468-3494`: ephemeral tool
/// progress (`isEphemeralToolProgress`) replaces the previous tick when the
/// LAST history message is a progress entry for the same tool call with the
/// same `data.type`; everything else appends and the consumer picks the
/// newest (`BashTool/UI.tsx:140` `.at(-1)`).
fn update_tool_use_progress_message(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    progress_tool_use_id: &str,
    progress: crate::types::message::ToolUseProgressMessage,
) {
    // CC `createProgressMessage` stamps `uuid: randomUUID()` (utils/messages.ts:617).
    // Progress is a `Message` member of the one history (batch D3, matching
    // CC types/message.ts:114): it feeds `buildMessageLookups`, the render
    // list drops the row itself (Messages.tsx:590), and the API projection
    // filters `progress` members (utils/messages.ts:2066-2068 →
    // `normalize_messages_for_api`).
    let new_message = crate::types::message::ProgressMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        tool_use_id: progress_tool_use_id.to_string(),
        parent_tool_use_id: progress_tool_use_id.to_string(),
        data: progress,
    };
    let mut entries = history_state.read().as_ref().clone();
    push_or_replace_progress_entry(&mut entries, new_message);
    history_state.set(Arc::new(entries));
}

/// Maps to: CC `REPL.tsx:3482-3494` — `last?.type === 'progress' &&
/// last.parentToolUseID === newMessage.parentToolUseID && last.data.type ===
/// newMessage.data.type` → replace the previous ephemeral tick in place;
/// otherwise append.
fn push_or_replace_progress_entry(
    entries: &mut Vec<HistoryEntry>,
    new_message: crate::types::message::ProgressMessage,
) {
    if crate::utils::session_storage::is_ephemeral_tool_progress(new_message.data.wire_type()) {
        if let Some(HistoryEntry::Message(Message::Progress(last))) = entries.last_mut() {
            if last.parent_tool_use_id == new_message.parent_tool_use_id
                && last.data.wire_type() == new_message.data.wire_type()
            {
                *last = new_message;
                return;
            }
        }
    }
    entries.push(HistoryEntry::Message(Message::Progress(new_message)));
}

#[derive(Default)]
struct ClearScreenOnGenerationHook {
    generation: u64,
    last_seen: Option<u64>,
}

impl Hook for ClearScreenOnGenerationHook {
    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        match self.last_seen {
            None => {
                self.last_seen = Some(self.generation);
            }
            Some(last_seen) if last_seen != self.generation => {
                // Maps to CC Ink forceRedraw(): clear the visible main screen
                // and repaint from a fresh retained frame, while preserving
                // scrollback for native mouse-wheel history navigation.
                updater.clear_screen();
                self.last_seen = Some(self.generation);
            }
            Some(_) => {}
        }
    }
}

fn resize_requires_clear_terminal(previous: (u16, u16), next: (u16, u16)) -> bool {
    let (prev_cols, prev_rows) = previous;
    let (next_cols, next_rows) = next;
    next_rows < prev_rows || (prev_cols != 0 && next_cols != prev_cols)
}

#[derive(Default)]
struct ClearTerminalOnResizeHook {
    cols: u16,
    rows: u16,
    last_seen: Option<(u16, u16)>,
}

impl Hook for ClearTerminalOnResizeHook {
    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        let current = (self.cols, self.rows);
        match self.last_seen {
            None => self.last_seen = Some(current),
            Some((last_cols, last_rows)) => {
                // Official CC Ink/log-update uses clearTerminal on width changes
                // and height shrink because wrapped native-scrollback rows cannot
                // be reliably patched after terminal geometry changes.
                if resize_requires_clear_terminal((last_cols, last_rows), current) {
                    updater.clear_terminal();
                }
                self.last_seen = Some(current);
            }
        }
    }
}

#[derive(Default, Props)]
struct ClearTerminalOnResizeProps {
    cols: u16,
    rows: u16,
}

#[component]
fn ClearTerminalOnResize(
    mut hooks: Hooks,
    props: &mut ClearTerminalOnResizeProps,
) -> impl Into<AnyElement<'static>> {
    let hook = hooks.use_hook(ClearTerminalOnResizeHook::default);
    hook.cols = props.cols;
    hook.rows = props.rows;
    element!(View(width: 0u32, height: 0u32))
}

#[derive(Default, Props)]
struct ClearScreenOnGenerationProps {
    generation: u64,
}

#[component]
fn ClearScreenOnGeneration(
    mut hooks: Hooks,
    props: &mut ClearScreenOnGenerationProps,
) -> impl Into<AnyElement<'static>> {
    let hook = hooks.use_hook(ClearScreenOnGenerationHook::default);
    hook.generation = props.generation;
    element!(View(width: 0u32, height: 0u32))
}

#[derive(Default)]
struct ClearLiveOutputOnMountHook {
    cleared: bool,
}

impl Hook for ClearLiveOutputOnMountHook {
    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        if !self.cleared {
            // Official /resume is a local command UI rendered in the main
            // screen. Clear only the retained live canvas before drawing the
            // selector; do not clear or purge terminal scrollback, so native
            // mouse-wheel scrolling of the terminal information flow keeps
            // working.
            updater.clear_terminal_output();
            self.cleared = true;
        }
    }
}

#[component]
fn ClearLiveOutputOnMount(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _ = hooks.use_hook(ClearLiveOutputOnMountHook::default);
    element!(View(width: 0u32, height: 0u32))
}

/// Maps to: CC `REPL.tsx:2186-2189,2209-2215` `setMessages(prev => [...prev,
/// createSystemMessage(text, level)])` — informational system notices are
/// ordinary `Message` members of the ONE history (batch D3); the API
/// projection drops them (`normalizeMessagesForAPI`, utils/messages.ts:2067)
/// and the render projection passes them through untouched.
fn push_system_notice(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    level: SystemMessageLevel,
    text: impl Into<String>,
) {
    let mut entries = history_state.read().as_ref().clone();
    entries.push(HistoryEntry::Message(Message::System(
        SystemMessage::informational(text, level),
    )));
    history_state.set(Arc::new(entries));
}

#[allow(clippy::too_many_arguments)]
fn messages_memo_key(
    messages: &Arc<Vec<RenderableMessage>>,
    conversation_id: u64,
    is_loading: bool,
    verbose: bool,
    hide_logo: bool,
    columns: u16,
    rows: u16,
    pending_permission_tool_use_id: Option<&str>,
    classifier_checking_tool_use_id: Option<&str>,
    classifier_checking_is_auto: bool,
    status_notice_context: &StatusNoticeContext,
    streaming_text: Option<&str>,
    in_progress_tool_use_ids: &std::collections::HashSet<String>,
    streaming_tool_use_ids: &std::collections::HashSet<String>,
    tools: &[crate::types::tools::Tool],
) -> String {
    messages_memo_key_for_screen(
        messages,
        conversation_id,
        is_loading,
        verbose,
        hide_logo,
        columns,
        rows,
        pending_permission_tool_use_id,
        classifier_checking_tool_use_id,
        classifier_checking_is_auto,
        status_notice_context,
        streaming_text,
        Screen::Prompt,
        false,
        in_progress_tool_use_ids,
        streaming_tool_use_ids,
        tools,
    )
}

#[allow(clippy::too_many_arguments)]
fn messages_memo_key_for_screen(
    messages: &Arc<Vec<RenderableMessage>>,
    conversation_id: u64,
    is_loading: bool,
    verbose: bool,
    hide_logo: bool,
    columns: u16,
    rows: u16,
    pending_permission_tool_use_id: Option<&str>,
    classifier_checking_tool_use_id: Option<&str>,
    classifier_checking_is_auto: bool,
    status_notice_context: &StatusNoticeContext,
    streaming_text: Option<&str>,
    screen: Screen,
    show_all_in_transcript: bool,
    in_progress_tool_use_ids: &std::collections::HashSet<String>,
    streaming_tool_use_ids: &std::collections::HashSet<String>,
    tools: &[crate::types::tools::Tool],
) -> String {
    // Maps to: CC `Messages.tsx:1064-1065`, which compares `inProgressToolUseIDs`
    // with `setsEqual` inside the memo comparator. Without it the subtree keeps
    // its retained tree when a tool starts or finishes, and every row's
    // static/dynamic decision (`should_render_statically`) goes stale.
    // `conversation_id` is a `Messages` prop in CC (`Messages.tsx:262`), so
    // `React.memo`'s default props comparison covers it there.
    let mut live_tool_use_ids = in_progress_tool_use_ids.iter().collect::<Vec<_>>();
    live_tool_use_ids.sort_unstable();
    let mut streaming_ids = streaming_tool_use_ids.iter().collect::<Vec<_>>();
    streaming_ids.sort_unstable();
    // Maps to: CC `Messages.tsx:1079-1087` — the `tools` case in
    // `areMessagesPropsEqual` compares the ordered NAME list, not the array
    // identity, so a rebuilt pool with the same names keeps the memo.
    let tool_pool = crate::components::messages_list::tool_pool_memo_key(tools);
    format!(
        "messages:{:p}:{}:{}:{:?}:{:?}:{}:{}:{}:{}:{}:{:?}:{:?}:{}:{:?}:{:?}:{:?}:{}:{:?}:{:?}:{}",
        Arc::as_ptr(messages),
        messages.len(),
        conversation_id,
        messages.first().map(|message| message.uuid.as_str()),
        messages.last().map(|message| message.uuid.as_str()),
        is_loading,
        verbose,
        hide_logo,
        columns,
        rows,
        pending_permission_tool_use_id,
        classifier_checking_tool_use_id,
        classifier_checking_is_auto,
        status_notice_context,
        streaming_text,
        screen,
        show_all_in_transcript,
        live_tool_use_ids,
        streaming_ids,
        tool_pool,
    )
}

/// Maps to: CC `screens/REPL.tsx:817-819`.
const TITLE_ANIMATION_FRAMES: [&str; 2] = ["⠂", "⠐"];
const TITLE_STATIC_PREFIX: &str = "✳";
const TITLE_ANIMATION_INTERVAL_MS: u64 = 960;

#[derive(Default, Props)]
struct AnimatedTerminalTitleProps {
    is_animating: bool,
    title: String,
    disabled: bool,
    no_prefix: bool,
}

/// Maps to: CC `screens/REPL.tsx:820-856` `AnimatedTerminalTitle`.
///
/// Sets the terminal tab title, with an animated prefix glyph while a query
/// is running. Isolated from REPL so the 960ms tick re-renders only this
/// null leaf instead of the entire REPL tree, exactly like the official
/// extraction note (:822-827). The interval stops — not merely slows — when
/// disabled, prefixless, idle, or the terminal loses focus (:841-849).
#[component]
fn AnimatedTerminalTitle(
    props: &AnimatedTerminalTitleProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let terminal_focused = hooks.use_terminal_focus();
    let frame = hooks.use_state(|| 0usize);
    let tick_active = !props.disabled && !props.no_prefix && props.is_animating && terminal_focused;
    let mut frame_for_tick = frame;
    hooks.use_interval(
        move || {
            frame_for_tick.set((frame_for_tick.get() + 1) % TITLE_ANIMATION_FRAMES.len());
        },
        tick_active.then(|| Duration::from_millis(TITLE_ANIMATION_INTERVAL_MS)),
    );
    let prefix = if props.is_animating {
        TITLE_ANIMATION_FRAMES[frame.get() % TITLE_ANIMATION_FRAMES.len()]
    } else {
        TITLE_STATIC_PREFIX
    };
    hooks.use_terminal_title_opt(if props.disabled {
        None
    } else if props.no_prefix {
        Some(props.title.clone())
    } else {
        Some(format!("{prefix} {}", props.title))
    });
    // CC returns null; the zero-size leaf is this file's null-component
    // convention (see ClearLiveOutputOnMount).
    element!(View(width: 0u32, height: 0u32))
}

#[allow(clippy::too_many_arguments)]
fn memoized_messages(
    messages: Arc<Vec<RenderableMessage>>,
    conversation_id: u64,
    is_loading: bool,
    verbose: bool,
    hide_logo: bool,
    columns: u16,
    rows: u16,
    pending_permission_tool_use_id: Option<String>,
    classifier_approvals: ClassifierApprovalsState,
    status_notice_context: StatusNoticeContext,
    streaming_text: Option<String>,
    in_progress_tool_use_ids: Arc<std::collections::HashSet<String>>,
    streaming_tool_use_ids: Arc<std::collections::HashSet<String>>,
    tools: Arc<Vec<crate::types::tools::Tool>>,
) -> AnyElement<'static> {
    memoized_messages_for_screen(
        messages,
        conversation_id,
        is_loading,
        verbose,
        hide_logo,
        columns,
        rows,
        pending_permission_tool_use_id,
        classifier_approvals,
        status_notice_context,
        streaming_text,
        Screen::Prompt,
        false,
        in_progress_tool_use_ids,
        streaming_tool_use_ids,
        tools,
    )
}

/// Maps to: CC `screens/REPL.tsx#REPL:5819-5843,6160-6190` Messages
/// construction sites. The screen-specific `Memo` key is the documented
/// iocraft identity carrier for their structurally distinct JSX parents.
#[allow(clippy::too_many_arguments)]
fn memoized_messages_for_screen(
    messages: Arc<Vec<RenderableMessage>>,
    conversation_id: u64,
    is_loading: bool,
    verbose: bool,
    hide_logo: bool,
    columns: u16,
    rows: u16,
    pending_permission_tool_use_id: Option<String>,
    classifier_approvals: ClassifierApprovalsState,
    status_notice_context: StatusNoticeContext,
    streaming_text: Option<String>,
    screen: Screen,
    show_all_in_transcript: bool,
    in_progress_tool_use_ids: Arc<std::collections::HashSet<String>>,
    streaming_tool_use_ids: Arc<std::collections::HashSet<String>>,
    // Maps to: CC `REPL.tsx:5821` / `:6162` `tools={tools}` — the memo half at
    // `REPL.tsx:1216`, the same object both Messages sites receive.
    tools: Arc<Vec<crate::types::tools::Tool>>,
) -> AnyElement<'static> {
    // Mirror official `Messages = React.memo(...)` before entering the
    // Messages component. The component keeps its own bailout as a safety
    // net for tests and direct callers.
    let memo_key = messages_memo_key_for_screen(
        &messages,
        conversation_id,
        is_loading,
        verbose,
        hide_logo,
        columns,
        rows,
        pending_permission_tool_use_id.as_deref(),
        classifier_approvals.checking_tool_use_id(),
        classifier_approvals.checking_is_auto(),
        &status_notice_context,
        streaming_text.as_deref(),
        screen,
        show_all_in_transcript,
        &in_progress_tool_use_ids,
        &streaming_tool_use_ids,
        &tools,
    );
    let classifier_checking_tool_use_id = classifier_approvals
        .checking()
        .map(|state| state.tool_use_id.clone());
    let classifier_checking_is_auto = classifier_approvals.checking_is_auto();

    // CC's prompt and legacy transcript Messages live under structurally
    // distinct JSX parents. Carry that identity through iocraft so returning
    // from a verbose transcript remounts row caches with prompt-mode props.
    element! {
        Memo(key: format!("messages-{screen:?}"), memo_key: memo_key, compare: memo_key_eq as MemoComparator) {
            Messages(
                messages: messages,
                conversation_id: conversation_id,
                is_loading: is_loading,
                verbose: verbose || screen == Screen::Transcript,
                screen: screen,
                show_all_in_transcript: show_all_in_transcript,
                hide_logo: hide_logo,
                pending_permission_tool_use_id: pending_permission_tool_use_id,
                classifier_checking_tool_use_id: classifier_checking_tool_use_id,
                classifier_checking_is_auto: classifier_checking_is_auto,
                status_notice_context: status_notice_context,
                streaming_text: streaming_text,
                in_progress_tool_use_ids: in_progress_tool_use_ids,
                streaming_tool_use_ids: streaming_tool_use_ids,
                // CC `REPL.tsx:5821` / `:6162` `tools={tools}`.
                tools: tools,
            )
        }
    }
    .into_any()
}

fn settings_tab_from_command(tab: LocalSettingsTab) -> SettingsTab {
    match tab {
        LocalSettingsTab::Status => SettingsTab::Status,
        LocalSettingsTab::Config => SettingsTab::Config,
        LocalSettingsTab::Usage => SettingsTab::Usage,
    }
}

fn local_command_panel_from_command(command: LocalCommandUi) -> LocalCommandPanel {
    match command {
        LocalCommandUi::PluginSettings {
            data,
            preceding_input_blocks,
        } => LocalCommandPanel::PluginSettings {
            data,
            preceding_input_blocks,
            immediate_execution: false,
        },
        LocalCommandUi::Settings { default_tab } => {
            LocalCommandPanel::Settings(settings_tab_from_command(default_tab))
        }
        LocalCommandUi::AddDir { args } => LocalCommandPanel::AddDir { args },
        LocalCommandUi::Tasks { context } => LocalCommandPanel::Tasks { context },
        LocalCommandUi::ResumePicker => LocalCommandPanel::Resume,
        LocalCommandUi::Help => LocalCommandPanel::Help,
        LocalCommandUi::Hooks => LocalCommandPanel::Hooks,
        LocalCommandUi::Model => LocalCommandPanel::Model,
        LocalCommandUi::Fast => LocalCommandPanel::Fast,
        LocalCommandUi::Theme => LocalCommandPanel::Theme,
        LocalCommandUi::Export {
            data,
            preceding_input_blocks,
        } => LocalCommandPanel::Export(data, preceding_input_blocks),
        LocalCommandUi::Permissions => LocalCommandPanel::Permissions,
        LocalCommandUi::Memory => LocalCommandPanel::Memory,
        LocalCommandUi::Agents => LocalCommandPanel::Agents,
        LocalCommandUi::Skills => LocalCommandPanel::Skills,
        LocalCommandUi::Stats => LocalCommandPanel::Stats,
        LocalCommandUi::Doctor => LocalCommandPanel::Doctor,
        LocalCommandUi::Diff => LocalCommandPanel::Diff,
        LocalCommandUi::Sandbox => LocalCommandPanel::Sandbox,
        LocalCommandUi::Btw { question, context } => LocalCommandPanel::Btw { question, context },
        LocalCommandUi::Copy { data } => LocalCommandPanel::Copy(data),
        LocalCommandUi::Mcp { args } => LocalCommandPanel::Mcp { args },
        LocalCommandUi::Login { args } => LocalCommandPanel::Login { args },
        LocalCommandUi::Logout { args } => LocalCommandPanel::Logout { args },
        LocalCommandUi::Ide { args } => LocalCommandPanel::Ide { args },
        LocalCommandUi::Effort {
            args,
            has_conversation_messages,
        } => LocalCommandPanel::Effort {
            args,
            has_conversation_messages,
        },
    }
}

fn local_command_ui_is_immediate(panel: &LocalCommandPanel) -> bool {
    // Mirrors upstream command metadata for the local command UI descriptors
    // currently wired in this UI-only main-screen port. The flag is still
    // meaningful in native scrollback: immediate command UI renders in the
    // bottom-slot equivalent while shouldHidePromptInput remains separate.
    matches!(
        panel,
        LocalCommandPanel::PluginSettings { .. }
            | LocalCommandPanel::Settings(SettingsTab::Status)
            | LocalCommandPanel::Hooks
            | LocalCommandPanel::Mcp { .. }
            | LocalCommandPanel::Sandbox
            | LocalCommandPanel::Btw { .. }
    ) || (matches!(
        panel,
        LocalCommandPanel::Model | LocalCommandPanel::Effort { .. }
    ) && crate::utils::immediate_command::should_inference_config_command_be_immediate())
}

fn local_command_ui_dismiss_result(panel: &LocalCommandPanel) -> Option<String> {
    match panel {
        // ValidatePlugin has no source Escape/dismiss callback.
        LocalCommandPanel::PluginSettings { .. } => None,
        // Settings.tsx owns Status/Usage Esc, while Config.tsx owns Config
        // list-mode Esc after search cedes focus.
        LocalCommandPanel::Settings(SettingsTab::Config) => {
            Some("Config dialog dismissed".to_string())
        }
        LocalCommandPanel::Settings(_) => Some("Status dialog dismissed".to_string()),
        LocalCommandPanel::AddDir { .. } => Some("Did not add a working directory.".to_string()),
        LocalCommandPanel::Tasks { .. } => Some("Background tasks dialog dismissed".to_string()),
        LocalCommandPanel::Resume => Some("Resume cancelled".to_string()),
        LocalCommandPanel::Help => Some("Help dialog dismissed".to_string()),
        LocalCommandPanel::Hooks => Some("Hooks dialog dismissed".to_string()),
        LocalCommandPanel::Model => Some(model::handle_cancel_output()),
        LocalCommandPanel::Effort { .. } => Some(effort::effort::handle_cancel_output()),
        LocalCommandPanel::Fast => Some("Kept Fast mode OFF".to_string()),
        LocalCommandPanel::Theme => Some(theme::handle_cancel_output()),
        LocalCommandPanel::Export(..) => Some("Export cancelled".to_string()),
        LocalCommandPanel::Permissions => Some("Permissions dialog dismissed".to_string()),
        LocalCommandPanel::Memory => Some("Cancelled memory editing".to_string()),
        LocalCommandPanel::Agents => Some("Agents dialog dismissed".to_string()),
        LocalCommandPanel::Skills => Some("Skills dialog dismissed".to_string()),
        LocalCommandPanel::Stats => Some("Stats dialog dismissed".to_string()),
        LocalCommandPanel::Doctor => Some("Claude Code diagnostics dismissed".to_string()),
        LocalCommandPanel::Diff => Some("Diff dialog dismissed".to_string()),
        LocalCommandPanel::Sandbox => Some("Sandbox dialog dismissed".to_string()),
        // Official `/btw` dismisses with `display: 'skip'` (no transcript row).
        LocalCommandPanel::Btw { .. } => None,
        LocalCommandPanel::Copy(_) => Some("Copy cancelled".to_string()),
        LocalCommandPanel::Mcp { .. } => Some("MCP dialog dismissed".to_string()),
        LocalCommandPanel::Login { .. } => Some("Login interrupted".to_string()),
        LocalCommandPanel::Logout { .. } => Some("Logout cancelled".to_string()),
        LocalCommandPanel::Ide { .. } => Some("IDE dialog dismissed".to_string()),
        LocalCommandPanel::It2Setup { .. } => {
            Some("Teammate spawn cancelled - iTerm2 setup required".to_string())
        }
    }
}

impl ActiveLocalCommandUi {
    fn from_slash_command(
        command: LocalCommandUi,
        invocation: SlashCommandInvocation,
        is_query_active: bool,
    ) -> Self {
        Self::from_slash_command_with_source(command, invocation, is_query_active, false)
    }

    fn from_slash_command_with_source(
        command: LocalCommandUi,
        invocation: SlashCommandInvocation,
        is_query_active: bool,
        from_keybinding: bool,
    ) -> Self {
        let mut panel = local_command_panel_from_command(command);
        if let LocalCommandPanel::PluginSettings {
            immediate_execution,
            ..
        } = &mut panel
        {
            // Source REPL.tsx:4302-4310: plugin.immediate is always true.
            *immediate_execution = is_query_active;
        }
        let is_immediate =
            local_command_ui_is_immediate(&panel) || (from_keybinding && is_query_active);
        Self {
            completion: Arc::new(std::sync::OnceLock::new()),
            invocation: Some(LocalCommandInvocation {
                slash_command: invocation,
                dismiss_result: local_command_ui_dismiss_result(&panel),
            }),
            panel,
            // Official processSlashCommand sets shouldHidePromptInput: true
            // for ordinary local command UI slash commands. When an immediate
            // command UI runs while a query is active, REPL executes it
            // through the direct immediate path with shouldHidePromptInput:
            // false so the prompt and notifications remain mounted.
            should_hide_prompt_input: !(is_immediate && is_query_active),
            is_local_command_ui: true,
            is_immediate,
        }
    }

    fn from_it2_setup_request(request: It2SetupPromptRequest) -> Self {
        Self {
            completion: Arc::new(std::sync::OnceLock::new()),
            panel: LocalCommandPanel::It2Setup { request },
            invocation: None,
            should_hide_prompt_input: true,
            is_local_command_ui: false,
            is_immediate: false,
        }
    }

    fn is_resume(&self) -> bool {
        matches!(self.panel, LocalCommandPanel::Resume)
    }
}

fn set_local_command_ui(
    current: Option<ActiveLocalCommandUi>,
    next: ActiveLocalCommandUi,
) -> Option<ActiveLocalCommandUi> {
    if matches!(current.as_ref(), Some(active) if active.is_local_command_ui)
        && !next.is_local_command_ui
    {
        return current;
    }
    Some(next)
}

fn clear_local_command_ui(current: Option<ActiveLocalCommandUi>) -> Option<ActiveLocalCommandUi> {
    match current {
        Some(active) if active.is_local_command_ui => None,
        other => other,
    }
}

fn preserve_local_command_ui_against_non_local_tool_update(
    current: Option<ActiveLocalCommandUi>,
) -> Option<ActiveLocalCommandUi> {
    // Mirrors official `setToolJSX`: while a local command UI is active,
    // ordinary tool UI updates cannot overwrite it unless the caller uses the
    // explicit clear-local path. Prompt help is not a local command invocation,
    // so it is not preserved by this guard.
    current.filter(|active| active.is_local_command_ui)
}

fn local_command_result_messages(
    command_name: &str,
    args: &str,
    output: &str,
    is_error: bool,
) -> Vec<RenderableMessage> {
    // CC processSlashCommand.tsx display:'system' branch: two
    // `createCommandInputMessage` rows (system subtype local_command) —
    // typed transcript entries. normalizeMessagesForAPI later converts these
    // to User text (messages.ts:2059-2093), retaining their UUID/timestamp.
    let output_tag = if is_error {
        crate::constants::xml::LOCAL_COMMAND_STDERR_TAG
    } else {
        crate::constants::xml::LOCAL_COMMAND_STDOUT_TAG
    };
    vec![
        RenderableMessage {
            uuid: Uuid::new_v4().to_string(),
            kind: RenderableMessageKind::System(SystemMessage::local_command(
                crate::utils::messages::format_command_input_tags(command_name, args),
            )),
        },
        RenderableMessage {
            uuid: Uuid::new_v4().to_string(),
            kind: RenderableMessageKind::System(SystemMessage::local_command(format!(
                "<{output_tag}>{output}</{output_tag}>"
            ))),
        },
    ]
}

fn model_visible_local_command_invocation_result_messages(
    invocation: &SlashCommandInvocation,
    output: &str,
) -> Vec<RenderableMessage> {
    // CC processSlashCommand.tsx model-turn branch: both rows are USER
    // messages carrying the wire text (formatCommandInputTags breadcrumb,
    // then the tagged stdout) — model-visible by type.
    vec![
        RenderableMessage::user(
            Uuid::new_v4().to_string(),
            crate::utils::messages::format_command_input_tags(
                &invocation.command_name,
                &invocation.args,
            ),
        ),
        RenderableMessage::user(
            Uuid::new_v4().to_string(),
            format!("<local-command-stdout>{output}</local-command-stdout>"),
        ),
    ]
}

fn explicit_local_model_messages(messages: &[RenderableMessage]) -> Vec<Message> {
    // Extract the explicit user rows of the compact command's prefix. This
    // does not decide API visibility: local_command system entries enter the
    // shared typed history and normalizeMessagesForAPI converts them to User.
    messages
        .iter()
        .filter_map(|message| match &message.kind {
            RenderableMessageKind::User {
                message: user_message,
            } => match user_message.first_content_block() {
                Some(crate::types::message::UserContent::Text(content)) => {
                    Some(Message::User(crate::types::message::UserMessage {
                        // Model projection of this row: reuse the row uuid.
                        uuid: message.uuid.clone(),
                        timestamp: chrono::Utc::now(),
                        content: vec![crate::types::message::UserContent::Text(content.clone())],
                        is_compact_summary: false,
                        plan_content: None,
                        image_paste_ids: None,
                        is_visible_in_transcript_only: false,
                        mcp_meta: None,
                        source_tool_assistant_uuid: None,
                        permission_mode: None,
                        origin: None,
                        summarize_metadata: None,
                    }))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn append_model_visible_local_command_result(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    invocation: &SlashCommandInvocation,
    output: &str,
) {
    // Both rows are USER messages — `history_entry_from_row` makes them
    // `Message` entries, so the one history carries render and model at once
    // (the old explicit model-history dual write collapsed here).
    let rows = model_visible_local_command_invocation_result_messages(invocation, output);
    append_history_rows(history_state, rows);
}

fn local_command_invocation_result_messages(
    invocation: &LocalCommandInvocation,
    output: &str,
    is_error: bool,
) -> Vec<RenderableMessage> {
    local_command_result_messages(
        &invocation.slash_command.command_name,
        &invocation.slash_command.args,
        output,
        is_error,
    )
}

fn local_command_ui_dismiss_messages(active: &ActiveLocalCommandUi) -> Vec<RenderableMessage> {
    let Some(invocation) = active.invocation.as_ref() else {
        return Vec::new();
    };
    let Some(result) = invocation.dismiss_result.as_deref() else {
        return Vec::new();
    };
    local_command_invocation_result_messages(invocation, result, false)
}

fn append_local_command_invocation_result(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    invocation: &LocalCommandInvocation,
    output: &str,
    is_error: bool,
) {
    append_history_rows(
        history_state,
        local_command_invocation_result_messages(invocation, output, is_error),
    );
}

fn append_local_command_ui_dismiss_result(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    active: &ActiveLocalCommandUi,
) {
    append_history_rows(history_state, local_command_ui_dismiss_messages(active));
}

/// Maps to: CC REPL `sandboxPermissionRequestQueue` entry (REPL.tsx:1537-1543).
/// The resolver is a field of the queue item (CC `resolvePromise`); the queue
/// itself is REPL-local `use_state`, not a global.
#[derive(Clone)]
struct LocalSandboxPermissionAsk {
    host_pattern: crate::utils::sandbox::sandbox_adapter::NetworkHostPattern,
    resolve: Arc<dyn Fn(bool) + Send + Sync>,
}

/// Maps to: CC REPL `sandboxAskCallback` body (REPL.tsx:2934-3056): swarm
/// worker mailbox forward first, otherwise the local dialog queue. `app_store`
/// is used only by the worker branch (CC :2967-2973 sets
/// `pendingSandboxRequest` via `setAppState`); local asks travel to the REPL
/// over `local_ask_tx` (CC: the callback closes over
/// `setSandboxPermissionRequestQueue`). Bridge fan-out (CC :2999-3052,
/// `BRIDGE_MODE`) is not ported; the entry keeps the multi-resolver shape so
/// it can land without reshaping the queue.
fn create_repl_sandbox_ask_callback(
    app_store: crate::state::store::AppStore,
    local_ask_tx: async_channel::Sender<LocalSandboxPermissionAsk>,
) -> crate::utils::sandbox::sandbox_adapter::SandboxAskCallback {
    Arc::new(
        move |host_pattern: crate::utils::sandbox::sandbox_adapter::NetworkHostPattern| {
            let app_store = app_store.clone();
            let local_ask_tx = local_ask_tx.clone();
            Box::pin(async move {
                let worker_forward = crate::hooks::tool_permission::handlers::swarm_worker_handler::begin_pending_sandbox_request(
                    &app_store,
                    &host_pattern.host,
                );
                repl_sandbox_ask_flow(&local_ask_tx, host_pattern, worker_forward).await
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        },
    )
}

/// Maps to: CC REPL `sandboxAskCallback` promise body (REPL.tsx:2936-2994).
/// Worker-forwarded asks resolve via the leader mailbox and never enter the
/// local queue (CC :2937-2975; a failed mailbox send yields `worker_forward:
/// None` and falls back to the local queue, CC :2947-2957). Local asks are
/// queued for the REPL dialog with a per-request resolver (CC
/// `resolvePromise`); the `bounded(1)` `try_send` doubles as CC's
/// `resolveOnce` guard. A closed carrier or dropped queue entry (REPL
/// unmounted) denies via `unwrap_or(false)` — the CC promise's only resolvers
/// are gone.
async fn repl_sandbox_ask_flow(
    local_ask_tx: &async_channel::Sender<LocalSandboxPermissionAsk>,
    host_pattern: crate::utils::sandbox::sandbox_adapter::NetworkHostPattern,
    worker_forward: Option<async_channel::Receiver<bool>>,
) -> bool {
    if let Some(rx) = worker_forward {
        return rx.recv().await.unwrap_or(false);
    }
    let (tx, rx) = async_channel::bounded(1);
    let ask = LocalSandboxPermissionAsk {
        host_pattern,
        resolve: Arc::new(move |allow| {
            let _ = tx.try_send(allow);
        }),
    };
    if local_ask_tx.send(ask).await.is_err() {
        return false;
    }
    rx.recv().await.unwrap_or(false)
}

/// Maps to: CC REPL.tsx:6292-6343 sandbox dialog `onUserResponse` over the
/// REPL-local `sandboxPermissionRequestQueue`. The optional settings persist
/// (CC :6302-6330) runs before resolution, then ALL pending asks for the same
/// host resolve with the user's answer and are removed while other hosts stay
/// queued (CC :6332-6343 — parallel same-domain requests). No current request
/// is a no-op (CC `if (!currentRequest) return`). Bridge sibling cleanups (CC
/// :6345-6354) are not ported (`BRIDGE_MODE` absent).
fn handle_local_sandbox_permission_response(
    queue: &mut Vec<LocalSandboxPermissionAsk>,
    response: crate::components::permissions::sandbox_permission_request::SandboxPermissionResponse,
    persist_to_settings: &mut dyn FnMut(&str, bool),
) {
    let Some(current) = queue.first() else {
        return;
    };
    let approved_host = current.host_pattern.host.clone();
    if response.persist_to_settings {
        persist_to_settings(&approved_host, response.allow);
    }
    for ask in queue
        .iter()
        .filter(|ask| ask.host_pattern.host == approved_host)
    {
        (ask.resolve)(response.allow);
    }
    queue.retain(|ask| ask.host_pattern.host != approved_host);
}

fn permission_worker_badge_to_ui(
    badge: crate::types::permissions::PermissionWorkerBadge,
) -> crate::components::permissions::worker_badge::WorkerBadgeProps {
    let color =
        badge
            .color
            .as_deref()
            .and_then(|value| match value.to_ascii_lowercase().as_str() {
                "black" => Some(Color::Black),
                "red" => Some(Color::Red),
                "green" => Some(Color::Green),
                "yellow" => Some(Color::Yellow),
                "blue" => Some(Color::Blue),
                "magenta" | "purple" => Some(Color::Magenta),
                "cyan" => Some(Color::Cyan),
                "white" => Some(Color::White),
                "grey" | "gray" => Some(Color::Grey),
                _ => None,
            });
    crate::components::permissions::worker_badge::WorkerBadgeProps {
        name: badge.name,
        color,
    }
}

/// The outcome of the prologue every dialog answer runs before it is allowed
/// to have any effect.
///
/// Maps to: CC `interactiveHandler.ts:160` — `if (!claim()) return` is a bare
/// early return in JS because the callback owns nothing after it; here the two
/// outcomes are named so the caller cannot fall through into the side effects
/// by accident.
enum PermissionAnswerClaim {
    /// CC's `return` from a failed `claim()`: another racer already resolved
    /// this row, so nothing at all happens — no disk write, no live-context
    /// write, no withdrawal (the winner owns that).
    Lost,
    /// CC's fall-through past `claim()`, carrying the response as
    /// `handleUserAllow` leaves it (`PermissionContext.ts:291-318`).
    Won(PermissionPromptResponse),
}

/// Which queued row a dialog answer belongs to.
///
/// Maps to: CC `components/permissions/PermissionRequest.tsx:196-231` — the
/// dialog is handed the ROW and every answer is a method on it
/// (`toolUseConfirm.onAllow(...)` / `.onReject()`), so CC cannot mis-address an
/// answer no matter what the queue did in between. `toolUseConfirmQueue[0]` is
/// read only to CHOOSE what to render (`REPL.tsx:6035,6038,6046`), for the
/// footer hint (`:1615`) and for the global Escape (`:2856`
/// `toolUseConfirmQueue[0]?.onAbort()`); no CC site resolves a decision by
/// index (ast-grep `toolUseConfirmQueue[0]` over `src/` — those seven hits are
/// the whole census).
///
/// This port's answer travels back as a value, so it names its row: the
/// dialog stamps the `toolUseID` it rendered
/// (`components/permissions/permission_request.rs`). Popping `queue[0]`
/// instead was wrong from the moment a row could be withdrawn between the
/// render and the keypress — a teammate abort
/// (`in_process_runner.rs#teammate_leader_dialog_sink`, #179) or a recheck
/// sweep resolving the head (`recheck_permission`, #218) both move the head,
/// and the user's "allow" then landed on the successor. The claim mechanism
/// cannot catch it: both rows are unclaimed, so the answer legitimately wins
/// the claim on the wrong one.
///
/// `None` id is the legacy/no-row answer (the props-default dialog, and direct
/// callers in tests): head of queue, the previous behaviour. A row named but
/// absent is DISCARDED rather than redirected — the row is gone precisely
/// because a racer already claimed and resolved it, so there is nothing left to
/// answer and the successor is not the user's decision.
fn permission_queue_index_for_answer(
    queue: &[ToolUseConfirm],
    tool_use_id: Option<&str>,
) -> Option<usize> {
    match tool_use_id {
        Some(tool_use_id) => queue
            .iter()
            .position(|entry| entry.tool_use_id() == tool_use_id),
        None => (!queue.is_empty()).then_some(0),
    }
}

/// Apply every queue updater a producer has already published, before the
/// answer path reads the queue.
///
/// Maps to: CC `inProcessRunner.ts:209-217` — the abort listener's two
/// statements:
///
/// ```ts
/// const onAbortListener = () => {
///   if (decisionMade) return
///   decisionMade = true                                       // :211
///   reportPermissionWait()
///   resolve({ behavior: 'ask', message: SUBAGENT_REJECT_MESSAGE })
///   setToolUseConfirmQueue(queue => queue.filter(item => item.toolUseID !== toolUseID))
/// }
/// ```
///
/// `decisionMade = true` is SYNCHRONOUS and lands on one event loop with the
/// dialog's key handler, so in CC an answer handled after an abort always finds
/// the decision already made and returns from `onAllow`'s `if (!claim()) return`
/// having done nothing.
///
/// This port cannot take that claim at the abort site: the row's responder is
/// built inside the leader sink's per-ask closure
/// (`interactive_handler.rs#create_repl_interactive_permission_sink`) and
/// escapes only onto the row that travels this FIFO, so
/// `in_process_runner.rs#teammate_leader_dialog_sink` — which holds the
/// `tool_use_id` and nothing else — publishes claim-and-withdraw as an UPDATER
/// (`SetToolUseConfirmQueueFn::claim_and_remove_from_queue`). The FIFO is
/// therefore this port's serialization point, and the answer path settles it
/// before addressing a row: any abort published before the keypress was handled
/// is applied first, exactly as CC's synchronous listener would have been. The
/// answer then finds the row gone and is discarded, which is the same outcome
/// as losing the claim.
///
/// Without this, the claim rode the channel to whenever the REPL's drain future
/// was next polled, and inside that window a user's answer could take the claim,
/// write its rules to `settings.local.json`, and only then discover that the
/// teammate waiting on the decision had already given up.
fn settle_pending_permission_queue_updaters(
    updaters: &async_channel::Receiver<
        crate::utils::swarm::leader_permission_bridge::ToolUseConfirmQueueUpdater,
    >,
    mut queue: Vec<ToolUseConfirm>,
) -> Vec<ToolUseConfirm> {
    while let Ok(updater) = updaters.try_recv() {
        queue = updater(queue);
    }
    queue
}

/// Settle the queue, address the row the answer names, and take its claim —
/// the prologue of CC's `onAllow` (`interactiveHandler.ts:154-160` /
/// `inProcessRunner.ts:250-257`) with this port's FIFO settled first.
///
/// One function rather than three calls at the use site so the order cannot be
/// got wrong: settling AFTER the lookup would read a queue that predates the
/// abort, and claiming before settling would hand the claim to the loser.
fn claim_permission_answer_from_queue(
    updaters: &async_channel::Receiver<
        crate::utils::swarm::leader_permission_bridge::ToolUseConfirmQueueUpdater,
    >,
    queue: &mut Vec<ToolUseConfirm>,
    response: PermissionPromptResponse,
    app_store: &crate::state::store::AppStore,
) -> Option<(ToolUseConfirm, PermissionPromptResponse)> {
    *queue = settle_pending_permission_queue_updaters(updaters, std::mem::take(queue));
    // CC answers the ROW the dialog was handed (`toolUseConfirm.onAllow`),
    // never a queue index — see `permission_queue_index_for_answer`.
    let index = permission_queue_index_for_answer(queue, response.tool_use_id.as_deref())?;
    let PermissionAnswerClaim::Won(response) =
        claim_permission_answer(&queue[index], response, app_store)
    else {
        return None;
    };
    Some((queue.remove(index), response))
}

/// The dismissal half of [`claim_permission_answer_from_queue`].
///
/// Maps to: CC `interactiveHandler.ts:184` `onReject` / `:138` `onAbort` — the
/// same `if (!claim()) return` prologue, for the same reason: a dismissal that
/// lost the race must not write the live context, must not resolve, and must
/// not drop the winner's row.
fn claim_permission_cancel_from_queue(
    updaters: &async_channel::Receiver<
        crate::utils::swarm::leader_permission_bridge::ToolUseConfirmQueueUpdater,
    >,
    queue: &mut Vec<ToolUseConfirm>,
    tool_use_id: Option<&str>,
) -> Option<ToolUseConfirm> {
    *queue = settle_pending_permission_queue_updaters(updaters, std::mem::take(queue));
    // CC `PermissionRequest.tsx:206-214` rejects the ROW the dialog holds
    // (`toolUseConfirm.onReject()`), so an Esc/Ctrl-C that arrives after that
    // row was withdrawn must not fall through onto its successor.
    let index = permission_queue_index_for_answer(queue, tool_use_id)?;
    if !queue[index].responder.claim() {
        return None;
    }
    Some(queue.remove(index))
}

/// Maps to: CC `hooks/toolPermission/handlers/interactiveHandler.ts:154-181`
/// `onAllow`'s prologue, and `inProcessRunner.ts:250-281`, which opens with the
/// same claim and then diverges (see below):
///
/// ```ts
/// async onAllow(updatedInput, permissionUpdates, feedback, contentBlocks) {
///   if (!claim()) return          // atomic check-and-mark before await
///   …
///   resolveOnce(await ctx.handleUserAllow(updatedInput, permissionUpdates, …))
/// }
/// ```
///
/// with `handleUserAllow` reaching `persistPermissions`
/// (`PermissionContext.ts:299-300` → `:139-147`).
///
/// The claim comes FIRST — ahead of the disk write AND ahead of
/// `setToolPermissionContext` — because an answer that lost the race to the
/// sweep's `recheckPermission` (`interactiveHandler.ts:204-231`) or to a
/// teammate's abort (`inProcessRunner.ts:210-216`) is not a decision at all in
/// CC. Answering while a recheck resolves used to write the rule to
/// `settings.local.json` and move the live permission context on the way to a
/// `respond()` nobody was left to receive.
///
/// This is a different ordering question from the two settled nearby, and
/// neither governs here:
/// - #159's fail-closed persist-then-project lives INSIDE `persistPermissions`
///   (`PermissionContext.ts:141-145`: `persistPermissionUpdates` throws before
///   `setToolPermissionContext` runs) and is untouched — it is called here
///   through `permission_context::persist_permissions`;
/// - 734f7ac's state-then-persist governs the SDK permission-prompt normalizer
///   (`PermissionPromptToolResultSchema.ts:95-106`), a different path with a
///   different CC site.
///
/// The persist-failure arm has no CC counterpart (CC's `persistPermissionUpdates`
/// is fire-and-forget); it is the port's fail-closed conversion of a failed
/// write into a denial plus a notification, and it stays behind the claim
/// because it, too, moves user-visible state.
///
/// The two CC builders persist through DIFFERENT functions, and which one runs
/// decides whether `preserveMode` can protect anything:
///
/// ```ts
/// // interactiveHandler.ts:172-181 -> handleUserAllow -> PermissionContext.ts:139-147
/// persistPermissionUpdates(updates)                       // disk
/// setToolPermissionContext(applyPermissionUpdates(...))   // AND the live context
/// // inProcessRunner.ts:263
/// persistPermissionUpdates(permissionUpdates)             // disk, and nothing else
/// ```
///
/// The teammate's row writes the leader's context back itself, at `:265-281`,
/// with `{ preserveMode: true }`. Running the persist-AND-apply form for it
/// instead lands the answer — `setMode` and all — in the leader's store first,
/// so `preserveMode`'s `prev.toolPermissionContext.mode` (`REPL.tsx:3108-3110`)
/// reads back the very value it exists to protect and preserves nothing. That
/// is what [`apply_permission_answer_to_leader_context`] is paired with here:
/// the disk write happens in this function, the context write happens there,
/// and the ordering between them is the whole point.
fn claim_permission_answer(
    confirm: &ToolUseConfirm,
    response: PermissionPromptResponse,
    app_store: &crate::state::store::AppStore,
) -> PermissionAnswerClaim {
    // CC `:160` `if (!claim()) return // atomic check-and-mark before await`.
    if !confirm.responder.claim() {
        return PermissionAnswerClaim::Lost;
    }
    if !matches!(
        response.choice,
        PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
    ) || response.permission_updates.is_empty()
    {
        // CC `PermissionContext.ts:140` `if (updates.length === 0) return false`
        // and `inProcessRunner.ts:265` `if (permissionUpdates.length > 0)`.
        return PermissionAnswerClaim::Won(response);
    }
    let persisted = if confirm.preserves_leader_permission_mode() {
        // CC `inProcessRunner.ts:263` — disk only.
        crate::utils::permissions::permission_update::persist_permission_updates(
            &response.permission_updates,
        )
        .map(|()| false)
    } else {
        // CC `PermissionContext.ts:141-145` — disk, then the live context, in
        // that order (#159 fail-closed).
        crate::hooks::tool_permission::permission_context::persist_permissions(
            app_store,
            &response.permission_updates,
        )
    };
    match persisted {
        Ok(_) => PermissionAnswerClaim::Won(response),
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!(
                "Could not persist permission response updates: {error:#}"
            ));
            let mut notifications =
                crate::context::notifications::NotificationsWriter::new(app_store.clone());
            notifications.add_notification(
                crate::context::notifications::Notification::text(
                    "permission-persist-failed",
                    format!("Could not persist permission: {error}"),
                    crate::context::notifications::NotificationPriority::Immediate,
                )
                .with_color(crate::context::notifications::NotificationColor::Error)
                .with_timeout_ms(5_000),
            );
            PermissionAnswerClaim::Won(
                PermissionPromptResponse::new(PermissionPromptChoice::Deny)
                    .with_feedback(format!("Permission was not saved: {error}")),
            )
        }
    }
}

/// The context write-back half of an answered row, run after
/// [`claim_permission_answer`] and before the row's `resolve`.
///
/// Maps to: CC `inProcessRunner.ts:265-281` for an in-process teammate's row —
///
/// ```ts
/// if (permissionUpdates.length > 0) {
///   const setToolPermissionContext = getLeaderSetToolPermissionContext()
///   if (setToolPermissionContext) {
///     const currentAppState = toolUseContext.getAppState()
///     const updatedContext = applyPermissionUpdates(currentAppState.toolPermissionContext, permissionUpdates)
///     setToolPermissionContext(updatedContext, { preserveMode: true })
///   }
/// }
/// ```
///
/// — and to `PermissionContext.ts:142-145` for every other row, which is the
/// same two statements with no options, so the incoming mode is adopted (CC's
/// comment at `REPL.tsx:3105-3107`: "User-initiated mode changes (e.g.,
/// selecting 'allow all edits') must NOT be overridden").
///
/// **The base is the leader's live context, where CC reads
/// `toolUseContext.getAppState()`, and that is deliberate.** For a teammate's
/// row CC's `getAppState` is `agentGetAppState` (`runAgent.ts:416-497`), a
/// closure that re-reads the parent store on EVERY call and re-applies the
/// transform; this port's carrier for the asker's context is a per-run SNAPSHOT
/// (`ToolUseConfirm::asking_tool_permission_context`, the deviation already
/// booked at `interactive_handler.rs#recheck_permission`). Substituting the
/// snapshot here would not be closer to CC — it would drop every leader-side
/// grant made after the row was raised, which CC's live read never does. What
/// the live leader context differs from CC's live TRANSFORMED context in is
/// exactly the four fields `agentGetAppState` rewrites: `mode` (`:421-434`),
/// `shouldAvoidPermissionPrompts` (`:440-451`),
/// `awaitAutomatedChecksBeforeDialog` (`:458-463`) and `alwaysAllowRules` when
/// `allowedTools` is set (`:469-479`, which REPLACES the map with
/// `{cliArg, session}`). Every one of those reaching the coordinator is the
/// leak `preserveMode` exists to stop — CC guards only the first; this port
/// declines to reproduce the other three rather than writing a worker's scoped
/// rule map over the leader's.
fn apply_permission_answer_to_leader_context(
    confirm: &ToolUseConfirm,
    app_store: &crate::state::store::AppStore,
    effective_request: &crate::types::permissions::PermissionRequest,
    response: &PermissionPromptResponse,
) {
    let (next_context, _decision) = apply_prompt_response(
        &app_store.tool_permission_context(),
        effective_request,
        response,
    );
    crate::utils::swarm::leader_permission_bridge::set_leader_tool_permission_context(
        app_store,
        next_context,
        if confirm.preserves_leader_permission_mode() {
            crate::utils::swarm::leader_permission_bridge::SetToolPermissionContextOptions::preserve_mode()
        } else {
            crate::utils::swarm::leader_permission_bridge::SetToolPermissionContextOptions::none()
        },
    );
}

fn send_mailbox_permission_prompt_response(
    target: &crate::types::permissions::MailboxPermissionResponseTarget,
    response: &PermissionPromptResponse,
) -> bool {
    let approved = matches!(
        response.choice,
        PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
    );
    let permission_updates =
        crate::utils::permissions::permission_update_schema::permission_updates_to_official_json(
            &response.permission_updates,
        );
    crate::utils::swarm::permission_sync::send_permission_response_via_mailbox(
        &target.worker_name,
        &crate::utils::swarm::permission_sync::PermissionResolution {
            decision: if approved { "approved" } else { "rejected" }.to_string(),
            resolved_by: "leader".to_string(),
            feedback: response.feedback.clone(),
            updated_input: approved.then(|| response.updated_input.clone()).flatten(),
            permission_updates: (approved && !permission_updates.is_empty())
                .then_some(permission_updates),
        },
        &target.request_id,
        target.team_name.as_deref(),
    )
}

/// Maps to: CC `screens/REPL.tsx` `resume(sessionId, log, entrypoint)`.
///
/// The caller runs this on a worker thread: lifecycle hooks may wait for child
/// processes, while retained state is replaced on the render thread only after
/// this ordered transaction succeeds.
fn resume(
    mut target: crate::commands::resume::ResumeTarget,
    initial_main_thread_agent_definition: Option<
        Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>,
    >,
    mut agent_definitions: Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult>,
    agent_type: Option<String>,
    model: Option<String>,
) -> Result<crate::utils::session_restore::ProcessedResume, String> {
    let session_id = target
        .metadata
        .session_id
        .clone()
        .unwrap_or_else(|| target.session_id.clone());
    let is_fork = target.entrypoint == Some(crate::types::command::ResumeEntrypoint::Fork);
    // CC REPL.tsx:2359 keeps the original log alongside deserializeMessages.
    // Preserve serialized slug metadata for both copyPlanForResume and Fork.
    let plan_messages = target
        .metadata
        .original_messages
        .clone()
        .unwrap_or_else(|| target.entries.clone());
    if is_fork {
        let deserialized =
            crate::utils::conversation::deserialize_messages_with_interrupt_detection(
                std::mem::take(&mut target.entries),
            );
        target.metadata.skill_restore =
            crate::utils::conversation::restore_skill_state_from_messages(&deserialized.messages);
        target.metadata.todos =
            crate::utils::session_restore::extract_todos_from_transcript(&deserialized.messages);
        target.entries = deserialized.messages;
        target.turn_interruption_state = deserialized.turn_interruption_state;
    }
    let mut loaded = crate::utils::session_restore::ResumeLoadResult::try_from(&target)?;
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::CoordinatorMode,
    ) {
        if let Some(warning) =
            crate::coordinator::coordinator_mode::match_session_mode(loaded.mode.as_deref())
        {
            let warning = Message::System(SystemMessage::informational(
                warning,
                SystemMessageLevel::Warning,
            ));
            Arc::make_mut(&mut loaded.messages).push(warning);
            loaded.renderable_messages =
                Arc::new(crate::utils::messages::normalize_messages(&loaded.messages));
            agent_definitions = Arc::new(
                crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides_readonly(
                    &crate::bootstrap::state::get_original_cwd(),
                ),
            );
        }
    }
    // CC `REPL.tsx:2398-2403` passes `getAppState: () => store.getState()` into
    // `executeSessionEndHooks`, so the session-derived hooks are merged
    // (`utils/hooks.ts:4119-4125` → `:3038-3046` → `getHooksConfig`). The key is
    // `getSessionId()`, hardcoded by `executeHooksOutsideREPL` (`:3040`) — the
    // session being left, read before the switch below. Gate mirrors CC `:1516`.
    let hooks_config = crate::services::hooks::load_hooks_config_with_session_hooks(
        &crate::bootstrap::state::get_session_id(),
    );
    if !crate::utils::env_utils::is_bare_mode() {
        let _ = crate::services::hooks::block_on_hook_future(
            crate::services::hooks::lifecycle::execute_session_end_hooks(
                &hooks_config,
                "resume",
                Vec::new(),
            ),
        );
    }
    // CC REPL.tsx:2407-2420: hooks see the old global identity and receive
    // the target identity explicitly. The switch happens only below.
    let hook_result_messages = crate::utils::session_start::process_session_start_hooks(
        "resume",
        Some(&session_id),
        agent_type.as_deref(),
        model.as_deref(),
    );
    let (hook_messages, hook_rows) =
        crate::utils::session_start::project_hook_result_messages(&hook_result_messages);
    let mut restored_entries = target.entries;
    restored_entries.extend(
        hook_result_messages
            .iter()
            .filter_map(|message| serde_json::to_value(message).ok()),
    );
    Arc::make_mut(&mut loaded.messages).extend(hook_messages);
    Arc::make_mut(&mut loaded.renderable_messages).extend(hook_rows);
    // Source calls are eager through slug assignment, then detached at await.
    let copying: std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> = if is_fork {
        Box::pin(crate::utils::plans::copy_plan_for_fork(
            &plan_messages,
            &session_id,
        ))
    } else {
        Box::pin(crate::utils::plans::copy_plan_for_resume(
            &plan_messages,
            Some(&session_id),
        ))
    };
    if let Some(runtime) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
        runtime.spawn(copying);
    } else {
        // Embedded callers without a published runtime still execute the
        // same detached copy; its thread owns the runtime until completion.
        std::thread::spawn(move || {
            let _ = crate::utils::process_runtime::block_on_from_sync(copying);
        });
    }
    let cwd = loaded.project_path.clone().unwrap_or_else(|| {
        crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string()
    });
    loaded.read_file_state = crate::utils::query_helpers::extract_read_files_from_messages(
        &restored_entries,
        &cwd,
        crate::utils::file_state_cache::READ_FILE_STATE_CACHE_SIZE,
    );
    loaded.bash_tools =
        crate::utils::query_helpers::extract_bash_tools_from_messages(&restored_entries);
    let processed = crate::utils::session_restore::ProcessedResume::from_load_result(
        loaded,
        initial_main_thread_agent_definition,
        agent_definitions,
    );
    crate::utils::concurrent_sessions::update_session_name(target.metadata.agent_name.as_deref());

    // CC :2465-2489: read target before save/reset, switch, reset pointer,
    // then clear/restore metadata. Worktree restoration follows the switch.
    let target_session_costs = crate::cost_tracker::get_stored_session_costs(&session_id);
    let _ = crate::cost_tracker::save_current_session_costs();
    crate::cost_tracker::reset_cost_state();
    let project_dir = target
        .metadata
        .full_path
        .as_deref()
        .and_then(|path| std::path::Path::new(path).parent())
        .map(std::path::Path::to_path_buf);
    crate::bootstrap::state::switch_session(session_id.clone(), project_dir);
    crate::utils::asciicast::rename_recording_for_session();
    crate::utils::session_storage::reset_session_file_pointer();
    crate::utils::session_storage::clear_session_metadata();
    crate::utils::session_storage::restore_session_metadata(
        &crate::utils::session_storage::SessionMetadataCache {
            session_id,
            custom_title: target.metadata.custom_title,
            tag: target.metadata.tag,
            agent_name: target.metadata.agent_name,
            agent_color: target.metadata.agent_color,
            agent_setting: target.metadata.agent_setting,
            mode: target.metadata.mode,
            worktree_session: target.metadata.worktree_session.clone(),
            last_prompt: None,
            pr_number: target.metadata.pr_number,
            pr_url: target.metadata.pr_url,
            pr_repository: target.metadata.pr_repository,
        },
    );
    if !is_fork {
        // CC REPL.tsx:2507-2510: hot resume restores the worktree after the
        // session switch and metadata restore; startup's owner has a different
        // lifecycle and must not be called here as a second resume algorithm.
        crate::utils::session_restore::exit_restored_worktree();
        crate::utils::session_restore::restore_worktree_for_resume(
            target.metadata.worktree_session.as_ref(),
        );
        crate::utils::session_storage::adopt_resumed_session_file()
            .map_err(|error| format!("Failed to adopt resumed session: {error}"))?;
    } else if let Some(worktree) = crate::utils::worktree::get_current_worktree_session() {
        crate::utils::session_storage::save_worktree_state(
            crate::utils::worktree::worktree_session_to_persisted_json(&worktree),
        );
    }
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::CoordinatorMode,
    ) {
        crate::utils::session_storage::save_mode(
            if crate::coordinator::coordinator_mode::is_coordinator_mode() {
                "coordinator"
            } else {
                "normal"
            },
        );
    }
    if let Some(costs) = target_session_costs.as_ref() {
        crate::cost_tracker::set_cost_state_for_restore(costs);
    }
    Ok(processed)
}

/// Seeds the single-history state from a loaded conversation.
///
/// Batch D3 item 6 (resolved): the cold transcript seeds as plain
/// [`HistoryEntry::Message`] entries — CC `REPL.tsx:1650`
/// `useState<MessageType[]>(initialMessages ?? [])`, where the render list is
/// `normalizeMessages` over the same array. The dual ModelOnly+Row prefix
/// died with the render-approximation parser
/// (`conversation_recovery.rs::messages_from_entries` is now a thin wrapper
/// over the ONE typed parse; hiding happens at render — see that module's
/// doc for the per-drop → render-side mapping).
///
/// Residual `Row` legs: rows the message projection does not produce are
/// render-only companions of flows that are still double-carried — today the
/// resume `SessionStart` hook notices (`project_hook_result_messages`
/// projects `Message::HookResult` model halves plus System-notice rows; CC
/// ships those as attachment messages, so that flow's convergence is the
/// hook-family alignment, kin to residue item 4). They splice after the
/// message block, preserving today's order.
fn seed_history_entries(
    model_messages: &[Message],
    rows: &[RenderableMessage],
) -> Vec<HistoryEntry> {
    let mut normalizer = crate::utils::messages::MessageNormalizer::default();
    let mut projected = Vec::new();
    for message in model_messages {
        normalizer.push(message, &mut projected);
    }
    let projected_uuids: std::collections::HashSet<&str> =
        projected.iter().map(|row| row.uuid.as_str()).collect();

    let mut entries: Vec<HistoryEntry> = model_messages
        .iter()
        .cloned()
        .map(HistoryEntry::Message)
        .collect();
    entries.extend(
        rows.iter()
            .filter(|row| !projected_uuids.contains(row.uuid.as_str()))
            .cloned()
            .map(HistoryEntry::Row),
    );
    entries
}

/// Maps to: CC `screens/REPL.tsx#resume` post-load state commit
/// (`setMessages`, restore stores, agent state, and input/session reset).
fn apply_resume_restore_result(
    history_state: &mut State<Arc<Vec<HistoryEntry>>>,
    pending_responses: &mut State<Vec<PendingRuntimeMessage>>,
    content_replacement_state: &mut State<
        crate::utils::tool_result_storage::ContentReplacementState,
    >,
    resume_restore_stores: &mut State<ResumeRestoreStores>,
    loaded_nested_memory_paths: &mut State<std::collections::HashSet<String>>,
    main_thread_agent_definition: &mut State<
        Option<Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinition>>,
    >,
    app_store: &crate::state::store::AppStore,
    result: crate::utils::session_restore::ProcessedResume,
) {
    // Maps to: CC `REPL.tsx:2390-2584` in-session resume replacement. Both
    // query and render projections switch atomically; startup resume uses the
    // same ProcessedResume before mount.
    app_store.replace_with(|state| result.apply_to_app_state(state));
    // CC REPL.tsx:2548-2558: hot forks preserve the live replacement state
    // (including frozen classification and already-applied tool IDs).
    let replacement_state = (result.resume_restore_stores.entrypoint
        != Some(crate::types::command::ResumeEntrypoint::Fork))
    .then(|| {
        let replacements = result
            .content_replacements
            .as_deref()
            .map(Vec::as_slice)
            .unwrap_or_default();
        crate::utils::tool_result_storage::provision_content_replacement_state(
            Some(result.messages.as_slice()),
            replacements,
        )
        .unwrap_or_default()
    });

    resume_restore_stores.set(result.resume_restore_stores.as_ref().clone());
    loaded_nested_memory_paths.set(std::collections::HashSet::new());
    main_thread_agent_definition.set(result.restored_agent_def.clone());
    pending_responses.set(Vec::new());
    history_state.set(Arc::new(seed_history_entries(
        &result.messages,
        &result.renderable_messages,
    )));
    if let Some(replacement_state) = replacement_state {
        content_replacement_state.set(replacement_state);
    }
}

/// Maps to: CC REPL building `systemPrompt` via `buildEffectiveSystemPrompt` +
/// merging `getCoordinatorUserContext` into `userContext`.
fn build_repl_query_prompt_contexts(
    overrides: &crate::utils::system_prompt::CliSystemPromptOverrides,
    default_system_prompt: crate::services::api::claude::SystemPrompt,
    main_thread_agent_definition: Option<
        &crate::tools::agent_tool::load_agents_dir::AgentDefinition,
    >,
    mcp_state: &crate::state::app_state_store::McpState,
) -> (
    crate::services::api::claude::SystemPrompt,
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
) {
    let system_prompt = overrides.apply_with_agent(
        default_system_prompt,
        main_thread_agent_definition,
        None,
        None,
    );

    let mcp_names: Vec<&str> = mcp_state
        .clients
        .iter()
        .map(|server| server.client.name.as_str())
        .collect();
    let scratchpad_dir = crate::utils::permissions::filesystem::is_scratchpad_enabled()
        .then(crate::utils::permissions::filesystem::get_scratchpad_dir);

    let mut user_context = crate::context::get_user_context();
    user_context.extend(
        crate::coordinator::coordinator_mode::get_coordinator_user_context(
            &mcp_names,
            scratchpad_dir.as_deref(),
        ),
    );

    (
        system_prompt,
        user_context,
        crate::context::get_system_context(),
    )
}

/// Maps to: CC `screens/REPL.tsx#REPL` launch props; mutable session state is
/// read separately from AppState just as in the source.
#[component]
pub fn Repl(props: &ReplProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // Shared immutable capture for the synchronous, async, inbox, and scheduled
    // query callbacks installed below. This remains the mount-time REPL prop;
    // it is never refreshed from AppState or disk.
    let thinking_config = Arc::new(props.thinking_config.clone());
    // The connection effect is NOT called here — it belongs to
    // `<McpConnectionManager>`, which this component mounts at the root of its
    // tree (CC `REPL.tsx:6134-6138`). P5 G10 moved the call out of `main.rs`
    // into this body and wrote the component but never mounted it, so the
    // component was dead code and its context had no publisher.
    let mcp_manager_store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    // Maps to: CC `screens/REPL.tsx` calling `useLspInitializationNotification()`
    // — another REPL-mounted poll that used to be a pre-mount spawn here.
    crate::hooks::notifs::use_lsp_initialization_notification::use_lsp_initialization_notification(
        &mut hooks,
        mcp_manager_store.clone(),
    );
    // The mount-only promote of the startup queue (CC
    // `context/notifications.tsx:292-296`) is NOT here: at the source that
    // effect lives inside `useNotifications`, so it rides every mount of every
    // caller. REPL used to own it, which is a mount effect CC's REPL does not
    // have.
    //
    // OSC 52 transport for command-owned clipboard actions (`/copy`). The
    // handle is acquired unconditionally to preserve hook order.
    let (command_stdout, _) = hooks.use_output();
    // Provider-less component harnesses do not have the launch snapshot that
    // production receives from replLauncher. Keep only that fallback mount-
    // stable; real `props.commands` must remain live for launcher/MCP updates.
    let fallback_commands = hooks.use_const(|| {
        #[cfg(test)]
        {
            Arc::new(crate::commands::declared_commands_for_tests())
        }
        #[cfg(not(test))]
        {
            Arc::new(crate::commands::get_commands(
                &crate::bootstrap::state::get_original_cwd(),
            ))
        }
    });
    let local_commands = if props.commands.is_empty() {
        fallback_commands.clone()
    } else {
        props.commands.clone()
    };
    // Maps to CC REPL.tsx:1224-1231: local -> plugins -> MCP, with
    // earlier names winning each merge. Subscribe to the refreshed plugin
    // array so a reload reaches dispatch and suggestions without remounting.
    let (_, plugin_commands) = crate::state::app_state::use_app_state(&mut hooks, |state| {
        (
            Arc::as_ptr(&state.plugins.commands) as usize,
            state.plugins.commands.clone(),
        )
    });
    let commands_with_plugins = crate::hooks::use_merged_commands::use_merged_commands(
        &mut hooks,
        local_commands,
        plugin_commands,
    );
    // Connection discovery updates the flat AppState array asynchronously;
    // the REPL never waits for it before mounting.
    // CC's REPL has no `useMcp` — no such hook exists at the source. The
    // component-facing MCP surface is `use_mcp_reconnect`/`use_mcp_toggle_enabled`
    // on the manager; what REPL needs here is an imperative writer, built from
    // the store it already holds.
    let runtime_mcp_context = mcp_manager_store
        .clone()
        .map(crate::state::app_state_store::McpWriter::new);
    // Maps to: CC `useAppState(s => s.mcp)` for the read half. `use_mcp` is
    // the write handle and carries no subscription of its own, so a
    // render-time read through it would never be woken by an MCP
    // connect/disconnect; the imperative call sites below keep using it.
    let mcp_commands =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.mcp.commands.clone());
    // The existing MCP state retains Vec snapshots. Preserve the array
    // between equal snapshots at this boundary, without metadata-only hashes
    // or changing the MCP state owner in the plugin-refresh batch.
    // iocraft useMemo only hashes deps; Command has no Hash and a partial
    // field hash loses executable content. Retain the complete existing Vec
    // snapshot in a ref, replacing its array only when that snapshot changes.
    let mut mcp_command_array = hooks.use_ref(|| Arc::new(Vec::<crate::commands::Command>::new()));
    let retained_mcp_commands = mcp_command_array.read().clone();
    let mcp_commands = if retained_mcp_commands.as_ref() == &mcp_commands {
        retained_mcp_commands
    } else {
        let updated = Arc::new(mcp_commands);
        mcp_command_array.set(updated.clone());
        updated
    };
    let merged_commands = crate::hooks::use_merged_commands::use_merged_commands(
        &mut hooks,
        commands_with_plugins,
        mcp_commands,
    );
    let commands = if props.disable_slash_commands {
        Arc::new(Vec::new())
    } else {
        merged_commands
    };
    let initial_tools = props.initial_tools.clone();
    let debug = props.debug;
    let system_prompt_overrides = crate::utils::system_prompt::CliSystemPromptOverrides {
        custom: props.system_prompt.clone(),
        append: props.append_system_prompt.clone(),
    };
    let initial_model_messages = props.initial_messages.clone().unwrap_or_default();
    let initial_renderable_messages = props
        .initial_renderable_messages
        .clone()
        .unwrap_or_default();
    let initial_model_messages_for_replacements = initial_model_messages.clone();
    let initial_content_replacements = props.initial_content_replacements.clone();
    let initial_file_history_snapshots = props.initial_file_history_snapshots.clone();
    let initial_agent_name = props.initial_agent_name.clone();
    let initial_agent_color = props.initial_agent_color.clone();
    let initial_main_thread_agent_definition = props.main_thread_agent_definition.clone();
    let initial_main_thread_agent_definition_for_state =
        initial_main_thread_agent_definition.clone();

    // Maps to CC `REPL.tsx:1650` `useState<MessageType[]>(initialMessages ??
    // [])` — the ONE conversation history; startup resume is present on the
    // first REPL render, not injected by a mount effect. The render rows and
    // the API history are projections of this single vec;
    // `messagesRef`'s eager-write wrapper is React-batching compensation that
    // iocraft's set-then-read State does not need.
    let mut history_state = hooks.use_state({
        let initial_model_messages = initial_model_messages.clone();
        let initial_renderable_messages = initial_renderable_messages;
        move || {
            Arc::new(seed_history_entries(
                &initial_model_messages,
                &initial_renderable_messages,
            ))
        }
    });
    // Maps to CC `REPL.tsx:2017` `const [conversationId, setConversationId] =
    // useState(randomUUID())`. CC mints a fresh UUID so `Messages.tsx:793`
    // row keys (`${msg.uuid}-${conversationId}`) change and stale memoized
    // rows remount after a compact reset (REPL.tsx:3461-3463, :3662-3664).
    // Cometix carries the same invalidation as a monotonic generation counter
    // in the row key/cache-key chain; a bump is CC's `randomUUID()` re-mint.
    let mut conversation_id = hooks.use_state(|| 0u64);
    // Maps to CC `REPL.tsx:1897`
    // `const [inProgressToolUseIDs, setInProgressToolUseIDs] = useState<Set<string>>(new Set())`.
    //
    // The REPL owns this set; tool execution writes it through the setter
    // `ToolUseContext` carries (`Tool.ts:227`). Cometix runs tool execution on
    // the query actor, so the writes arrive as `QueryEvent::SetInProgressToolUse`
    // deltas instead of in-process `setState` calls.
    let mut in_progress_tool_use_ids =
        hooks.use_state(|| Arc::new(std::collections::HashSet::<String>::new()));
    // Maps to CC `REPL.tsx:1900` `hasInterruptibleToolInProgressRef`. This
    // ref is read by `handlePromptSubmit` before it decides whether a new
    // prompt may abort the current query.
    let has_interruptible_tool_in_progress =
        hooks.use_const(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));
    // Maps to CC `REPL.tsx:1255` `useState<StreamingToolUse[]>`, reduced to the
    // id set `Messages.tsx:690` actually consumes. Covers the window between a
    // tool_use block reaching the transcript and its id entering
    // `in_progress_tool_use_ids` (`MessageRow.tsx:85-88`).
    let mut streaming_tool_use_ids =
        hooks.use_state(|| Arc::new(std::collections::HashSet::<String>::new()));
    let mut main_thread_agent_definition =
        hooks.use_state(move || initial_main_thread_agent_definition_for_state);
    // Maps to: CC `REPL.tsx:1570-1574` — the Haiku tab-title state and the
    // one-shot generation gate. The gate is a ref (not state) in CC; the
    // atomic mirrors that non-rendering write. Seeded true on resume
    // (initialMessages present) so a resumed session is not re-titled from
    // mid-conversation context.
    let haiku_title = hooks.use_state(|| Option::<String>::None);
    let haiku_title_attempted = hooks.use_const({
        let seeded = !initial_model_messages.is_empty();
        move || std::sync::Arc::new(std::sync::atomic::AtomicBool::new(seeded))
    });
    // Maps to: CC `REPL.tsx:939-942` mount-time env gate.
    let terminal_title_disabled = hooks.use_const(|| {
        crate::utils::env_utils::is_env_truthy(
            std::env::var("CLAUDE_CODE_DISABLE_TERMINAL_TITLE")
                .ok()
                .as_deref(),
        )
    });
    // CC fires generateSessionTitle as a void promise from onQueryImpl. The
    // Rust submit handler runs on whatever executor drives the render loop,
    // so the fire-and-forget ride shares the REPL's resident-consumer channel
    // pattern instead of assuming a tokio context.
    let haiku_title_channel =
        hooks.use_const(|| std::sync::Arc::new(async_channel::unbounded::<String>()));
    let haiku_title_tx = haiku_title_channel.0.clone();
    hooks.use_future({
        let haiku_title_rx = haiku_title_channel.1.clone();
        let mut haiku_title_for_generation = haiku_title;
        let haiku_title_attempted_for_generation = Arc::clone(&haiku_title_attempted);
        async move {
            while let Ok(text) = haiku_title_rx.recv().await {
                // The production render loop is tokio-driven; a mock render
                // loop (futures executor) has no reactor and therefore no
                // network domain — treat it as the failure path below.
                if tokio::runtime::Handle::try_current().is_err() {
                    haiku_title_attempted_for_generation
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    continue;
                }
                let abort = crate::tool::AbortController::default();
                match crate::utils::session_title::generate_session_title(&text, &abort).await {
                    Some(title) => haiku_title_for_generation.set(Some(title)),
                    // Maps to CC's rejection/None handler: reopen the gate so
                    // the next submission retries.
                    None => haiku_title_attempted_for_generation
                        .store(false, std::sync::atomic::Ordering::SeqCst),
                }
            }
        }
    });
    // Maps to: CC `REPL.tsx:1560-1577` — rename title wins over the agent
    // name, which wins over the Haiku topic; all fall back to the product
    // name. REPL still computes titleDisabled/terminalTitle itself (the haiku
    // generation gate reads them, :1591-1594); the OSC write and the 960ms
    // busy animation live in `AnimatedTerminalTitle` in the render tree.
    let terminal_title_from_rename = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.settings.terminal_title_from_rename != Some(false)
    });
    let session_tab_title = if terminal_title_from_rename {
        crate::utils::session_storage::get_current_session_metadata().custom_title
    } else {
        None
    };
    let terminal_tab_title = session_tab_title
        .or_else(|| {
            main_thread_agent_definition
                .read()
                .as_ref()
                .map(|agent| agent.agent_type.clone())
        })
        .or_else(|| haiku_title.read().clone())
        .unwrap_or_else(|| "Claude Code".to_string());
    // Maps to CC lazy `provisionContentReplacementState(initialMessages,
    // initialContentReplacements)` initialization.
    let mut content_replacement_state = hooks.use_state(move || {
        crate::utils::tool_result_storage::provision_content_replacement_state(
            Some(initial_model_messages_for_replacements.as_slice()),
            initial_content_replacements
                .as_deref()
                .map(Vec::as_slice)
                .unwrap_or_default(),
        )
        .unwrap_or_default()
    });
    let mut pending_responses = hooks.use_state(Vec::<PendingRuntimeMessage>::new);
    let mut permissions_continuation = hooks.use_state(|| None::<LocalCommandContinuation>);
    let mut streaming_text_preview = hooks.use_state(|| Option::<StreamingTextPreview>::None);
    // Maps to CC `REPL.tsx` `streamMode` + `responseLengthRef`.
    let mut stream_mode = hooks.use_state(|| SpinnerMode::Responding);
    let mut response_length_ref = hooks.use_ref(|| 0usize);
    let mut stop_hook_spinner_state = hooks.use_state(StopHookSpinnerState::default);
    // Test-only actor transport: exercise the real REPL consumer and keyboard
    // path with a held query channel, without starting an external model.
    #[cfg(test)]
    let active_query_probe = hooks
        .try_use_context::<tests::ReplActiveQueryProbe>()
        .map(|probe| probe.clone());
    #[cfg(test)]
    let initial_query_for_test = active_query_probe
        .as_ref()
        .filter(|probe| probe.start_active)
        .map(|probe| probe.handle.clone());
    let mut active_query = hooks.use_state(move || {
        #[cfg(test)]
        {
            initial_query_for_test
        }
        #[cfg(not(test))]
        {
            Option::<QueryHandle>::None
        }
    });
    // Partial useClaudeCodeHintRecommendation/usePluginRecommendationBase:
    // retain the existing 250ms pending-slot poll, but resolve on the published
    // process runtime. Source void promises outlive the initiating component;
    // only their State delivery is mount-scoped (existing REPL channel carrier).
    let mut hint_recommendation = hooks.use_state(|| {
        Option::<crate::utils::plugins::hint_recommendation::PluginHintRecommendation>::None
    });
    let hint_resolution_tasks = hooks.use_const(|| {
        Arc::new(async_channel::unbounded::<
            tokio::task::JoinHandle<
                Option<crate::utils::plugins::hint_recommendation::PluginHintRecommendation>,
            >,
        >())
    });
    let hint_is_checking = hooks.use_ref(|| false);
    hooks.use_future({
        let tasks = hint_resolution_tasks.1.clone();
        let mut hint_recommendation = hint_recommendation;
        let mut hint_is_checking = hint_is_checking;
        async move {
            while let Ok(task) = tasks.recv().await {
                // usePluginRecommendationBase: then(setRecommendation),
                // catch(logError), finally(isCheckingRef = false), in order.
                match task.await {
                    Ok(Some(resolved)) => hint_recommendation.set(Some(resolved)),
                    Ok(None) => {}
                    Err(error) => crate::utils::log::log_error(crate::utils::log::LogError::new(
                        error.to_string(),
                    )),
                }
                hint_is_checking.set(false);
            }
        }
    });
    hooks.use_interval(
        {
            let hint_recommendation = hint_recommendation;
            let mut hint_is_checking = hint_is_checking;
            let tasks = hint_resolution_tasks.0.clone();
            move || {
                if crate::bootstrap::state::get_is_remote_mode()
                    || hint_recommendation.read().is_some()
                    || hint_is_checking.get()
                {
                    return;
                }
                let Some(hint) = crate::utils::claude_code_hints::get_pending_hint_snapshot()
                else {
                    return;
                };
                let Some(runtime) =
                    crate::utils::process_runtime::runtime_handle_for_detached_work()
                else {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                        "Process runtime unavailable for plugin hint resolution".to_string(),
                    ));
                    return;
                };
                hint_is_checking.set(true);
                let task = runtime.spawn(async move {
                    let resolved =
                        crate::utils::plugins::hint_recommendation::resolve_plugin_hint(&hint)
                            .await;
                    if let Some(resolved) = &resolved {
                        crate::utils::debug::log_for_debugging(&format!(
                            "[useClaudeCodeHintRecommendation] surfacing {} from {}",
                            resolved.plugin_id, resolved.source_command,
                        ));
                        crate::utils::claude_code_hints::mark_shown_this_session();
                    }
                    // Existing native store uses value snapshots; JS reference
                    // identity for two equal replacement hints remains partial.
                    if crate::utils::claude_code_hints::get_pending_hint_snapshot().as_ref()
                        == Some(&hint)
                    {
                        crate::utils::claude_code_hints::clear_pending_hint();
                    }
                    resolved
                });
                if tasks.try_send(task).is_err() {
                    // Dropping a JoinHandle detaches the source promise. Its
                    // store effects still finish if this component unmounted.
                    hint_is_checking.set(false);
                }
            }
        },
        Some(Duration::from_millis(250)),
    );
    // Maps to CC `/compact` sharing its abortController with the forked
    // summary request so the ordinary cancel owner can stop compaction.
    let mut active_compact_abort = hooks.use_state(|| Option::<crate::tool::AbortController>::None);
    // Maps to CC `userInputOnProcessing`: non-empty only between accepting a
    // submit and installing its visible user row/query state.
    let mut user_input_on_processing = hooks.use_state(|| Option::<String>::None);
    // Maps to CC `/context` using `process.stdout.columns || 80`; the same
    // terminal-size hook also owns main-screen responsive rendering below.
    let (terminal_cols, terminal_rows) = hooks.use_terminal_size();
    let mut permission_queue = hooks.use_state(Vec::<ToolUseConfirm>::new);
    // Maps to: CC REPL.tsx:1537-1543 `sandboxPermissionRequestQueue` useState —
    // REPL-local queue of network-host asks. The per-request resolver lives on
    // each entry (CC `resolvePromise`); no global registry.
    let sandbox_permission_request_queue = hooks.use_state(Vec::<LocalSandboxPermissionAsk>::new);
    // Maps to: CC REPL.tsx:972 `useAppState(s => s.toolPermissionContext)` —
    // the permission context lives in the unified AppStore (mode changes must
    // flow through the on_change_app_state choke point). CC's REPL always runs
    // inside `<AppStateProvider>` (REPL.tsx is mounted by App.tsx), so the read
    // is strict: no fallback store, no provider-less path (Contract B clause 2).
    let app_store = crate::state::app_state::use_app_state_store(&mut hooks);
    crate::hooks::use_manage_plugins::use_manage_plugins(
        &mut hooks,
        !crate::bootstrap::state::get_is_remote_mode(),
    );
    // Maps to: CC REPL.tsx `useKickOffCheckAndDisable*PermissionsIfNeeded`.
    crate::utils::permissions::bypass_permissions_killswitch::use_kick_off_check_and_disable_bypass_permissions_if_needed(&mut hooks);
    crate::utils::permissions::bypass_permissions_killswitch::use_kick_off_check_and_disable_auto_mode_if_needed(&mut hooks);
    // Maps to CC `const queuedCommands = useCommandQueue()`. The reactive
    // snapshot keeps showSpinner mounted between queued task notifications.
    let queued_commands = crate::hooks::use_command_queue::use_command_queue(&mut hooks);
    let _apply_initial_launch_state = hooks.use_const({
        let app_store = app_store.clone();
        let initial_main_thread_agent_definition = initial_main_thread_agent_definition.clone();
        move || {
            let mut restored_file_history = None;
            if let Some(snapshots) = initial_file_history_snapshots.as_deref() {
                crate::utils::file_history::file_history_restore_state_from_log(
                    snapshots,
                    |state| restored_file_history = Some(Arc::new(state)),
                );
            }
            let has_initial_agent_identity =
                initial_agent_name.is_some() || initial_agent_color.is_some();
            let standalone = crate::utils::session_restore::compute_standalone_agent_context(
                initial_agent_name.as_deref(),
                initial_agent_color.as_deref(),
            );
            // B3 flip-audit: CC restores these via the pre-mount initialState
            // spread (sessionRestore.ts:543-549) — no store event exists on
            // this path. All-conditions-false must stay a Same, or the mount
            // would emit a notification CC never has. When any condition IS
            // set, the one post-mount notification this emits has no CC
            // counterpart either (Contract B pre-mount writer family) — full
            // pre-provider silent adoption lands at P5.
            app_store.set_state(|prev| {
                if restored_file_history.is_none()
                    && !has_initial_agent_identity
                    && initial_main_thread_agent_definition.is_none()
                {
                    return crate::state::store::UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                if let Some(file_history) = restored_file_history.as_ref() {
                    next.file_history = file_history.clone();
                }
                if has_initial_agent_identity {
                    next.standalone_agent_context = standalone.clone();
                }
                if let Some(agent) = initial_main_thread_agent_definition.as_ref() {
                    next.agent = Some(agent.agent_type.clone());
                }
                crate::state::store::UpdateDecision::Replace {
                    next: Arc::new(next),
                    result: (),
                }
            });
        }
    });
    // Maps to: CC passing `setAppState` into task framework helpers.
    crate::utils::task::framework::bind_task_app_store(Some(app_store.clone()));
    // Maps to: CC `useCanUseTool.tsx:55-79` — `useCanUseTool` closes over
    // `setToolUseConfirmQueue`, and the REPL passes the resulting `canUseTool`
    // into `query(...)` (`REPL.tsx:3137`, `:3409`), which is how a subagent's
    // permission ask reaches this queue (`AgentTool.tsx:399` → `:879` →
    // `runAgent.ts:753`). Cometix evaluates the ask inside each query actor, off
    // the render thread, so what reaches this component over a channel is CC's
    // queue UPDATER (`leaderPermissionBridge.ts:16-18`) — appends, withdrawals
    // and the recheck sweep all travel this one FIFO, which is what keeps
    // "append the row, then withdraw the row" ordered the way CC's single
    // synchronous setter is. Transport shape as `sandbox_ask_channel` below.
    let tool_use_confirm_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<
            crate::utils::swarm::leader_permission_bridge::ToolUseConfirmQueueUpdater,
        >())
    });
    // Maps to: CC `REPL.tsx:1645-1648` — `registerLeaderToolUseConfirmQueue(
    // setToolUseConfirmQueue)` on mount, `unregisterLeaderToolUseConfirmQueue()`
    // on unmount. `setToolUseConfirmQueue` is a React state UPDATER, so the
    // Rust applier ships the updater to the task that owns the queue state and
    // applies it there. One applier for every producer keeps CC's ordering: a
    // row is appended and later withdrawn through the same FIFO.
    let set_tool_use_confirm_queue = hooks.use_const({
        let confirm_tx = tool_use_confirm_channel.0.clone();
        move || {
            crate::utils::swarm::leader_permission_bridge::SetToolUseConfirmQueueFn::new(
                move |updater| {
                    let _ = confirm_tx.try_send(updater);
                },
            )
        }
    });
    let interactive_permission_sink = hooks.use_const({
        let set_tool_use_confirm_queue = set_tool_use_confirm_queue.clone();
        move || {
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                set_tool_use_confirm_queue,
            )
        }
    });
    let _leader_permission_bridge_registration = hooks.use_state({
        let set_tool_use_confirm_queue = set_tool_use_confirm_queue.clone();
        let app_store_for_bridge = app_store.clone();
        move || {
            crate::utils::swarm::leader_permission_bridge::LeaderPermissionBridgeRegistration::register(
                set_tool_use_confirm_queue,
                app_store_for_bridge,
            )
        }
    });
    hooks.use_future({
        let confirm_rx = tool_use_confirm_channel.1.clone();
        let mut permission_queue = permission_queue;
        async move {
            while let Ok(updater) = confirm_rx.recv().await {
                // CC `useCanUseTool.tsx:307-324` → `interactiveHandler.ts`
                // `setToolUseConfirmQueue(prev => [...prev, entry])`, and the
                // withdrawals/rechecks that go through the same setter
                // (`inProcessRunner.ts:214-216`, `REPL.tsx:3120-3125`). A
                // subagent's prompt is the same `ToolUseConfirm` in the same
                // queue (`REPL.tsx:1529`) as the parent's own tools; only the
                // resolver behind `onAllow`/`onReject` differs.
                let previous = permission_queue.read().clone();
                let next = updater(previous.clone());
                // React bails out when an updater hands back the state it was
                // given (`Object.is`), which is exactly what the recheck sweep
                // does (`REPL.tsx:3120-3125` returns `currentQueue`). Without
                // this, every permission-context write would re-render.
                if next != previous {
                    permission_queue.set(next);
                }
            }
        }
    });
    // Maps to: CC `SandboxManager.initialize(sandboxAskCallback)`
    // (REPL.tsx:3087-3094) + the callback's local-queue branch
    // (REPL.tsx:2979-2994). The ask callback runs on producer threads (network
    // proxy), so asks reach this component over a channel and land in State —
    // CC's callback closes over `setSandboxPermissionRequestQueue` directly.
    let sandbox_ask_channel = hooks
        .use_const(|| std::sync::Arc::new(async_channel::unbounded::<LocalSandboxPermissionAsk>()));
    hooks.use_future({
        let sandbox_ask_rx = sandbox_ask_channel.1.clone();
        let mut sandbox_permission_request_queue = sandbox_permission_request_queue;
        async move {
            while let Ok(ask) = sandbox_ask_rx.recv().await {
                // CC REPL.tsx:2988-2994
                // `setSandboxPermissionRequestQueue(prev => [...prev, entry])`.
                let mut queue = sandbox_permission_request_queue.read().clone();
                queue.push(ask);
                sandbox_permission_request_queue.set(queue);
            }
        }
    });
    // Held in hook state so the registration lifetime matches this component
    // (overlay.rs `OverlayRegistration` precedent): unmount clears the ask
    // callback and drops the queue/channel, so a parked producer denies
    // (`recv` Err → false) instead of hanging on a stale entry.
    let _sandbox_ask_registration = hooks.use_state({
        let store = app_store.clone();
        let sandbox_ask_tx = sandbox_ask_channel.0.clone();
        move || {
            crate::utils::sandbox::sandbox_adapter::SandboxAskCallbackRegistration::register(
                create_repl_sandbox_ask_callback(store, sandbox_ask_tx),
            )
        }
    });
    // Alias kept for call-site clarity: permission reads/writes go through
    // AppStore (CC useAppState + useSetAppState), not a transitional lens.
    let permission_store = app_store.clone();
    let mut inbox_poller_state =
        hooks.use_state(crate::hooks::use_inbox_poller::InboxPollerState::default);
    let mut classifier_approvals = hooks.use_state(ClassifierApprovalsState::default);
    let initial_resume_restore_stores = props.initial_resume_restore_stores.clone();
    let mut resume_restore_stores =
        hooks.use_state(move || initial_resume_restore_stores.as_ref().clone());
    // Maps to CC REPL's session-scoped `loadedNestedMemoryPathsRef`. Unlike
    // readFileState's bounded LRU, this set is non-evicting until clear,
    // compact, or an in-session resume switches the conversation.
    let mut loaded_nested_memory_paths = hooks.use_state(std::collections::HashSet::<String>::new);

    // Maps to: CC `screens/REPL.tsx:2643` — REPL owns the hook-local status
    // and passes it down to PromptInput/Notifications.
    let api_key_verification = use_api_key_verification(&mut hooks);
    let api_key_status = api_key_verification.status;
    // Maps to: CC `screens/REPL.tsx:5061-5064,5452-5455` `onInit`: startup
    // verification begins after REPL mounts, rather than in main/bootstrap.
    let startup_api_key_verification = api_key_verification.clone();
    hooks.use_effect(
        move || {
            drop(startup_api_key_verification.reverify());
        },
        (),
    );

    let prompt_submit = hooks.use_const({
        let commands = commands.clone();
        move || HandlePromptSubmit::new(production_deps(), commands)
    });
    // Maps to: CC `REPL.tsx` `useScheduledTasks` — session cron scheduler core.
    let cron_scheduler = hooks.use_const(|| {
        std::sync::Arc::new(std::sync::Mutex::new({
            let mut scheduler =
                crate::hooks::use_scheduled_tasks::create_repl_cron_scheduler(false);
            scheduler.start();
            scheduler
        }))
    });
    let startup_dialog_snapshot = hooks
        .try_use_context::<ReplStartupDialogSnapshot>()
        .map(|context| (*context).clone())
        .unwrap_or_default();
    let status_notice_context = hooks
        .try_use_context::<StatusNoticeContext>()
        .map(|context| (*context).clone())
        .unwrap_or_default();
    let external_editor_runtime = hooks
        .try_use_context::<crate::utils::prompt_editor::ExternalEditorRuntime>()
        .map(|runtime| *runtime);
    let keybinding_runtime_for_reload = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let keybinding_runtime_for_command_handlers = keybinding_runtime_for_reload.clone();
    let keybinding_runtime_for_transcript_display = keybinding_runtime_for_reload.clone();
    let channel_permission_callbacks =
        crate::state::app_state::use_app_state(&mut hooks, |state| {
            state.channel_permission_callbacks.clone()
        });
    let initial_show_remote_callout = startup_dialog_snapshot
        .remote_callout
        .as_ref()
        .is_some_and(should_show_remote_callout);
    let initial_show_desktop_upsell = startup_dialog_snapshot
        .desktop_upsell
        .as_ref()
        .is_some_and(should_show_desktop_upsell_startup);

    let mut active_local_command_ui = hooks.use_state(|| Option::<ActiveLocalCommandUi>::None);
    let resume_processing_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<(
            Option<LocalCommandInvocation>,
            Option<String>,
            Result<crate::utils::session_restore::ProcessedResume, String>,
            Option<ActiveLocalCommandUi>,
        )>())
    });
    let resume_processing_tx = resume_processing_channel.0.clone();
    let resume_processing_rx = resume_processing_channel.1.clone();
    let context_processing_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<(
            SlashCommandInvocation,
            Result<String, String>,
        )>())
    });
    let context_processing_tx = context_processing_channel.0.clone();
    let context_processing_rx = context_processing_channel.1.clone();
    hooks.use_future(async move {
        while let Ok((invocation, result)) = context_processing_rx.recv().await {
            let (output, is_error) = match result {
                Ok(output) => (output, false),
                Err(error) => (format!("Failed to analyze context: {error}"), true),
            };
            append_local_command_invocation_result(
                &mut history_state,
                &LocalCommandInvocation {
                    slash_command: invocation,
                    dismiss_result: None,
                },
                &output,
                is_error,
            );
            user_input_on_processing.set(None);
        }
    });
    let speculation_acceptance_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<SpeculationAcceptanceOutcome>())
    });
    let speculation_acceptance_tx = speculation_acceptance_channel.0.clone();
    let speculation_acceptance_rx = speculation_acceptance_channel.1.clone();
    hooks.use_future({
        let app_store = app_store.clone();
        async move {
            while let Ok(outcome) = speculation_acceptance_rx.recv().await {
                let is_complete = matches!(
                    outcome
                        .result
                        .as_ref()
                        .and_then(|result| result.boundary.as_ref()),
                    Some(crate::state::app_state_store::CompletionBoundary::Complete { .. })
                );
                let mut clean_messages = outcome.clean_messages;
                if !is_complete {
                    while matches!(clean_messages.last(), Some(Message::Assistant(_))) {
                        clean_messages.pop();
                    }
                }
                let feedback_message = outcome.result.as_ref().and_then(|result| {
                    crate::services::prompt_suggestion::speculation::create_speculation_feedback_message(
                        &clean_messages,
                        result.boundary.as_ref(),
                        result.time_saved_ms,
                        app_store.get().speculation_session_time_saved_ms,
                    )
                });
                let user_message = Message::User(crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(
                        outcome.input.clone(),
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
                let mut injected_messages = clean_messages.clone();
                injected_messages.extend(feedback_message);
                let transcript_values =
                    crate::utils::session_storage::typed_messages_as_transcript_values(
                        &clean_messages,
                    );
                let cwd = outcome
                    .active
                    .cache_safe_params
                    .tool_use_context
                    .effective_cwd();
                let extracted = crate::utils::query_helpers::extract_read_files_from_messages(
                    &transcript_values,
                    &cwd.to_string_lossy(),
                    crate::utils::file_state_cache::READ_FILE_STATE_CACHE_SIZE,
                );
                if !extracted.is_empty() {
                    let mut stores = resume_restore_stores.read().clone();
                    let mut cache = crate::utils::file_state_cache::FileStateCache::from_entries(
                        stores.read_file_state,
                    );
                    for entry in extracted {
                        let path = std::path::PathBuf::from(&entry.path);
                        cache.set(&path, entry);
                    }
                    stores.read_file_state = cache.entries_lru_to_mru();
                    resume_restore_stores.set(stores);
                }

                // Speculation-injected completed turns append as plain
                // `Message` entries — CC appends the accepted messages whole
                // and decides visibility at render (its
                // `prepareMessagesForInjection`, speculation.ts:203-272, is
                // model-side API hygiene, not a render projection). The old
                // dual-carrier ModelOnly+Row split existed only for the
                // hydrating projection's row drops, which the render-list
                // filter and the per-block components now own.
                let mut entries = history_state.read().as_ref().clone();
                entries.extend(
                    injected_messages
                        .iter()
                        .cloned()
                        .map(HistoryEntry::Message),
                );
                let entries = Arc::new(entries);
                history_state.set(Arc::clone(&entries));
                let history = history_model_messages(&entries);
                let rendered = project_history_rows(&entries);
                // B3 flip-audit: CC's single accept-time clear
                // (speculation.ts:851-857) guards on text==null &&
                // promptId==null → return prev. This outcome-side clear is
                // the second half of Cometix's async split (the submit-side
                // clear already ran), so with the guard it is silent at
                // runtime and the notification count matches CC.
                app_store.set_state(|prev| {
                    if prev.prompt_suggestion.text.is_none()
                        && prev.prompt_suggestion.prompt_id.is_none()
                    {
                        return crate::state::store::UpdateDecision::Same(());
                    }
                    let mut next = (**prev).clone();
                    next.prompt_suggestion = crate::state::app_state_store::PromptSuggestionState::default();
                    crate::state::store::UpdateDecision::Replace {
                        next: Arc::new(next),
                        result: (),
                    }
                });

                if is_complete {
                    if let Some(pipelined) = outcome.active.pipelined_suggestion.clone() {
                        app_store.replace_with(|state| {
                            state.prompt_suggestion =
                                crate::state::app_state_store::PromptSuggestionState {
                                    text: Some(pipelined.text.clone()),
                                    prompt_id: Some(pipelined.prompt_id.clone()),
                                    shown_at: current_time_millis(),
                                    accepted_at: 0,
                                    generation_request_id: pipelined.generation_request_id.clone(),
                                };
                        });
                        let mut cache =
                            Arc::unwrap_or_clone(outcome.active.cache_safe_params.clone());
                        let mut augmented = cache.fork_context_messages.as_ref().clone();
                        augmented.push(user_message);
                        augmented.extend(clean_messages);
                        cache.fork_context_messages = Arc::new(augmented);
                        tokio::spawn(async move {
                            let _ =
                                crate::services::prompt_suggestion::speculation::start_speculation(
                                    pipelined.text,
                                    cache,
                                    true,
                                )
                                .await;
                        });
                    }
                } else {
                    let cache = outcome.active.cache_safe_params;
                    let mut tool_use_context = cache.tool_use_context.clone();
                    tool_use_context.messages = history.clone();
                    tool_use_context.app_store = crate::tool::AppStoreRef::new(app_store.clone());
                    tool_use_context.read_file_state.replace(
                        resume_restore_stores.read().read_file_state.clone(),
                    );
                    tool_use_context.loaded_nested_memory_paths =
                        loaded_nested_memory_paths.read().clone();
                    tool_use_context.resume_restore_stores = resume_restore_stores.read().clone();
                    let params = crate::query::QueryParams {
                        turn_id: Uuid::new_v4().to_string(),
                        // The accepted user turn is already in typed history;
                        // CC calls `onQuery([])` for this continuation.
                        input: String::new(),
                        messages: rendered,
                        model_messages: history,
                        system_prompt: cache.system_prompt.clone(),
                        user_context: cache.user_context.clone(),
                        system_context: cache.system_context.clone(),
                        query_source: crate::constants::query_source::QuerySource::Prompt,
                        token_budget: None,
                        task_budget: None,
                        max_turns: None,
                        tool_use_context,
                    };
                    stream_mode.set(SpinnerMode::Responding);
                    response_length_ref.set(0);
                    active_query.set(Some(spawn_query(params, production_deps())));
                }
                user_input_on_processing.set(None);
            }
        }
    });
    // Maps to: CC REPL.tsx:1110-1113 `const [ideSelection, setIDESelection] =
    // useState<IDESelection | undefined>(undefined)` — REPL-local ownership.
    let mut ide_selection = hooks.use_state(|| Option::<IdeSelection>::None);
    // Maps to: CC REPL.tsx:1239-1242 `useIdeSelection(mcp.clients,
    // setIDESelection)`. CC upstream quirk preserved (do NOT "fix"): REPL
    // passes `mcp.clients` — NOT the merged `mcpClients` — so only the
    // live-managed MCP connections ever carry a selection handler. Cometix
    // registers the sink with the live connection registry
    // (`CONNECTED_CLIENTS`), which holds exactly those live-managed
    // connections; entry removal drops the per-connection sink clone
    // (useIdeSelection.ts:148 "No cleanup needed") and identity changes send
    // the CC-shaped reset object through this channel
    // (useIdeSelection.ts:73-83).
    //
    // SEAM (recorded, not implemented in this slice):
    // - CC feeds `ideSelection` into handlePromptSubmit → processUserInput →
    //   attachments (getSelectedLinesFromIDE / getOpenedFileFromIDE,
    //   attachments.ts:1615-1629); Cometix's selection is display-only today.
    // - CC's 4-state `useIdeConnectionStatus` (connected/pending/disconnected/
    //   null) and the disconnected-notification family remain unwired; only
    //   the strict connected gate exists here.
    let ide_selection_channel =
        hooks.use_const(|| std::sync::Arc::new(async_channel::unbounded::<IdeSelection>()));
    let _ide_selection_sink_registration = hooks.use_const({
        let sink = ide_selection_channel.0.clone();
        move || crate::services::mcp::client::register_ide_selection_sink(sink)
    });
    let ide_selection_rx = ide_selection_channel.1.clone();
    hooks.use_future(async move {
        // Live State handle captured by the recv loop (no snapshot): each
        // delivered value — real selection or identity-change reset object —
        // maps to CC `onSelect(selection)` / `setIDESelection(selection)`.
        while let Ok(selection) = ide_selection_rx.recv().await {
            ide_selection.set(Some(selection));
        }
    });
    let compact_processing_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<(
            SlashCommandInvocation,
            Result<crate::commands::compact::compact::CompactCommandResult, String>,
        )>())
    });
    let compact_processing_tx = compact_processing_channel.0.clone();
    let compact_processing_rx = compact_processing_channel.1.clone();
    hooks.use_future(async move {
        while let Ok((invocation, result)) = compact_processing_rx.recv().await {
            match result {
                Ok(command_result) => {
                    let crate::commands::compact::compact::CompactCommandResult {
                        compaction_result: mut result,
                        display_text: output,
                    } = command_result;
                    let mut restored = resume_restore_stores.read().clone();
                    restored.read_file_state = result.rebuilt_read_file_state.clone();
                    resume_restore_stores.set(restored);
                    loaded_nested_memory_paths.set(std::collections::HashSet::new());
                    let rows = model_visible_local_command_invocation_result_messages(
                        &invocation,
                        &output,
                    );
                    let command_messages = explicit_local_model_messages(&rows);
                    result
                        .messages_to_keep
                        .get_or_insert_with(Vec::new)
                        .extend(command_messages);
                    let post_messages =
                        crate::services::compact::compact::build_post_compact_messages(&result);
                    // CC's manual /compact APPENDS the post-compact block to
                    // the one array (REPL.tsx:3901 via
                    // processSlashCommand.tsx:915-921) — it never replaces it.
                    // Pre-compact rows stay in the array and are hidden by the
                    // render-time boundary slice (Messages.tsx:579-584), which
                    // verbose and fullscreen deliberately skip so both can
                    // still show them; query.ts:365 slices the same array for
                    // the API. Replacing the array here looked equivalent on a
                    // plain main screen — those rows survive in native
                    // scrollback — but it destroyed them for ctrl+o/verbose,
                    // for fullscreen (whose alt buffer has no native
                    // scrollback), and for anything else reading the full
                    // in-memory history. The stream-path reset
                    // (`setMessages(() => [newMessage])` REPL.tsx:3458) and
                    // partial compact (REPL.tsx:6990) are separate CC paths and
                    // do not license this one.
                    {
                        let mut entries = history_state.read().as_ref().clone();
                        entries.extend(post_messages.into_iter().map(HistoryEntry::Message));
                        history_state.set(Arc::new(entries));
                    }
                    // CC REPL.tsx:3662-3664: bump conversationId so row keys
                    // change and stale memoized rows remount.
                    conversation_id += 1;
                }
                Err(error) => {
                    append_local_command_invocation_result(
                        &mut history_state,
                        &LocalCommandInvocation {
                            slash_command: invocation,
                            dismiss_result: None,
                        },
                        &format!("Error during compaction: {error}"),
                        true,
                    );
                }
            }
            active_compact_abort.set(None);
            stream_mode.set(SpinnerMode::Responding);
            response_length_ref.set(0);
            user_input_on_processing.set(None);
        }
    });
    let plan_processing_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<(
            SlashCommandInvocation,
            crate::commands::plan::plan::PlanInspectionResult,
        )>())
    });
    let plan_processing_tx = plan_processing_channel.0.clone();
    let plan_processing_rx = plan_processing_channel.1.clone();
    hooks.use_future(async move {
        while let Ok((invocation, result)) = plan_processing_rx.recv().await {
            let output = match result {
                crate::commands::plan::plan::PlanInspectionResult::Output(output) => output,
                crate::commands::plan::plan::PlanInspectionResult::Open(path) => {
                    let editor_result = match external_editor_runtime {
                        Some(runtime) => runtime.edit_file(&path).await,
                        None => crate::utils::prompt_editor::EditorResult::default(),
                    };
                    match editor_result.error {
                        Some(error) => format!("Failed to open plan in editor: {error}"),
                        None => format!("Opened plan in editor: {}", path.display()),
                    }
                }
            };
            append_model_visible_local_command_result(&mut history_state, &invocation, &output);
            user_input_on_processing.set(None);
        }
    });
    let keybindings_processing_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<(
            SlashCommandInvocation,
            Result<crate::commands::keybindings::keybindings::KeybindingsFileResult, String>,
        )>())
    });
    let keybindings_processing_tx = keybindings_processing_channel.0.clone();
    let keybindings_processing_rx = keybindings_processing_channel.1.clone();
    let app_store_for_keybindings_reload = app_store.clone();
    let keybinding_runtime_for_editor_reload = keybinding_runtime_for_reload.clone();
    hooks.use_future(async move {
        while let Ok((invocation, prepared)) = keybindings_processing_rx.recv().await {
            let (output, is_error) = match prepared {
                Ok(crate::commands::keybindings::keybindings::KeybindingsFileResult::Disabled) => (
                    crate::commands::keybindings::keybindings::DISABLED_MESSAGE.to_string(),
                    false,
                ),
                Ok(crate::commands::keybindings::keybindings::KeybindingsFileResult::WriteDisabled { path }) => (
                    crate::commands::keybindings::keybindings::write_disabled_message(&path),
                    true,
                ),
                Ok(crate::commands::keybindings::keybindings::KeybindingsFileResult::Ready {
                    path,
                    file_exists,
                }) => {
                    let editor_result = match external_editor_runtime {
                        Some(runtime) => runtime.edit_file(&path).await,
                        None => crate::utils::prompt_editor::EditorResult::default(),
                    };
                    let loaded = crate::keybindings::load_user_bindings::reload_keybindings_sync_with_warnings();
                    if let Some(runtime) = keybinding_runtime_for_editor_reload.as_ref() {
                        runtime.replace_bindings(loaded.bindings);
                    }
                    crate::keybindings::keybinding_provider_setup::sync_keybinding_warning_notification(
                        &app_store_for_keybindings_reload,
                        &loaded.warnings,
                    );
                    (
                        crate::commands::keybindings::keybindings::editor_result_message(
                            &path,
                            file_exists,
                            editor_result.error.as_deref(),
                        ),
                        false,
                    )
                }
                Err(error) => (error, true),
            };
            append_local_command_invocation_result(
                &mut history_state,
                &LocalCommandInvocation {
                    slash_command: invocation,
                    dismiss_result: None,
                },
                &output,
                is_error,
            );
            user_input_on_processing.set(None);
        }
    });
    // Seed AppState.showRemoteCallout once from the startup snapshot (CC
    // main/bridge sets the field; REPL only reads/dismisses via AppState).
    let _seed_remote_callout = hooks.use_const({
        let app_store = app_store.clone();
        move || {
            if initial_show_remote_callout {
                // B3 flip-audit: CC's only set-true point guards
                // `if (prev.showRemoteCallout) return prev` (bridge.tsx:88).
                // The seed's initial value is always false, so this is a
                // zero-cost semantic alignment (guard never fires today).
                app_store.set_state(|prev| {
                    if prev.show_remote_callout {
                        return crate::state::store::UpdateDecision::Same(());
                    }
                    let mut next = (**prev).clone();
                    next.show_remote_callout = true;
                    crate::state::store::UpdateDecision::Replace {
                        next: Arc::new(next),
                        result: (),
                    }
                });
            }
        }
    });
    let show_remote_callout =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.show_remote_callout);
    // Maps to CC `REPL.tsx` `showDesktopUpsellStartup` local state.
    let mut show_desktop_upsell_startup = hooks.use_state(move || initial_show_desktop_upsell);
    // Maps to CC `REPL.tsx` `exitFlow` state. The safe Rust path renders
    // `ExitFlow` and lets the app loop exit, but never calls
    // `gracefulShutdown` or `process.exit` directly.
    let mut exit_flow_active = hooks.use_state(|| false);
    // Maps to: CC REPL.tsx:2011 `isMessageSelectorVisible`. The option list
    // itself is derived inside MessageSelector via use_memo keyed on the
    // messages Arc pointer (CC useMemo([messages]) reference semantics).
    let mut message_selector_visible = hooks.use_state(|| false);
    // Maps to: CC REPL `screen` + `showAllInTranscript`.
    let mut screen = hooks.use_state(Screen::default);
    let mut show_all_in_transcript = hooks.use_state(|| false);
    let mut should_exit = hooks.use_state(|| false);
    let process_exit_code = hooks
        .try_use_context::<Arc<std::sync::atomic::AtomicI32>>()
        .map(|code| code.clone());
    let mut redraw_generation = hooks.use_state(|| 0u64);
    let mut prompt_input_generation = hooks.use_state(|| 0u64);
    // Maps to: CC REPL-owned controlled `input`; PromptInput may unmount while
    // a local command panel is shown, but unsent text survives remount.
    let mut prompt_input_snapshot = hooks.use_state(String::new);
    let mut restored_input =
        hooks.use_state(|| Option::<crate::components::prompt_input::PromptInputTextUpdate>::None);
    // Maps to: CC REPL.tsx:1406 `isPromptInputActive` — true while the user is
    // actively typing a non-empty prompt; interrupt dialogs (sandbox ask) are
    // suppressed while active (REPL.tsx:2691-2692).
    let is_prompt_input_active = hooks.use_state(|| false);
    // Maps to: CC REPL.tsx:1862-1870 — the suppression timer is reset on every
    // input change and deactivates PROMPT_SUPPRESSION_MS after typing stops.
    // L1 carrier (PORTING.md `Resettable one-shot JS timer → deadline poll`):
    // CC's resettable setTimeout(1500) + clearTimeout-on-change becomes a
    // last-change Instant plus a 250ms poll, so deactivation fires within
    // [1500, 1750)ms of the last keystroke (CC: exactly 1500ms). Activation
    // is set in on_input_change (State::set) and becomes observable one frame
    // later than CC's same-commit setState re-render.
    let prompt_input_last_change = hooks.use_state(Instant::now);
    hooks.use_interval(
        {
            let mut is_prompt_input_active = is_prompt_input_active;
            let prompt_input_last_change = prompt_input_last_change;
            move || {
                if is_prompt_input_active.get()
                    && prompt_input_last_change.get().elapsed()
                        >= Duration::from_millis(PROMPT_SUPPRESSION_MS)
                {
                    is_prompt_input_active.set(false);
                }
            }
        },
        Some(Duration::from_millis(250)),
    );
    // Maps to CC REPL.tsx:1831-1870 setInputValue and its activity effect.
    // Both restore entry points run this setter before PromptInput's echo.
    let set_input_value = move |text: String| {
        let mut snapshot = prompt_input_snapshot;
        let mut active = is_prompt_input_active;
        let mut last_change = prompt_input_last_change;
        active.set(!text.trim().is_empty());
        if *snapshot.read() != text {
            last_change.set(Instant::now());
        }
        snapshot.set(text);
    };
    hooks.use_future({
        let app_store = app_store.clone();
        let haiku_title_attempted_for_resume = Arc::clone(&haiku_title_attempted);
        let mut haiku_title_for_resume = haiku_title;
        async move {
            while let Ok((invocation, success_message, result, picker)) = resume_processing_rx.recv().await
            {
                let active = active_local_command_ui.read().clone();
                let resume_succeeded = result.is_ok();
                match result {
                    Ok(result) => {
                        apply_resume_restore_result(
                            &mut history_state,
                            &mut pending_responses,
                            &mut content_replacement_state,
                            &mut resume_restore_stores,
                            &mut loaded_nested_memory_paths,
                            &mut main_thread_agent_definition,
                            &app_store,
                            result,
                        );
                        // Maps to: CC `REPL.tsx:2494-2495` — resumed sessions
                        // shouldn't re-title from mid-conversation context,
                        // and the previous session's Haiku title shouldn't
                        // carry over.
                        haiku_title_attempted_for_resume
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        haiku_title_for_resume.set(None);
                        user_input_on_processing.set(None);
                        active_query.set(None);
                        streaming_text_preview.set(None);
                        stop_hook_spinner_state.set(StopHookSpinnerState::default());
                        in_progress_tool_use_ids.set(Arc::new(std::collections::HashSet::new()));
                        streaming_tool_use_ids.set(Arc::new(std::collections::HashSet::new()));
                        let next_id = conversation_id.read().wrapping_add(1);
                        conversation_id.set(next_id);
                        set_input_value(String::new());
                        restored_input.set(None);
                        let next_generation = prompt_input_generation.read().wrapping_add(1);
                        prompt_input_generation.set(next_generation);
                        if let (Some(message), Some(invocation)) = (success_message, invocation.as_ref()) {
                            // CC branch.ts:285-286: onDone(system) only after
                            // resume's state commit, through the canonical
                            // local_command input/stdout projection.
                            let output = crate::utils::process_user_input::process_slash_command::system_display_local_command_result(
                                None,
                                &invocation.slash_command.command_name,
                                &invocation.slash_command.args,
                                &message,
                            );
                            append_history_rows(&mut history_state, output.messages);
                        }
                    }
                    Err(message) => {
                        if let Some(picker) = picker.as_ref() {
                            // CC resume.tsx:233-235: resume failure settles this
                            // invocation only after context.resume rejects.
                            crate::utils::log::log_error(crate::utils::log::LogError::new(&message));
                            if picker.completion.set(()).is_ok() {
                                if let Some(invocation) = picker.invocation.as_ref() {
                                    append_local_command_invocation_result(
                                        &mut history_state, invocation,
                                        &format!("Failed to resume: {message}"), false,
                                    );
                                }
                            }
                        } else if let Some(invocation) = invocation.as_ref().or_else(|| {
                            active
                                .as_ref()
                                .and_then(|active| active.invocation.as_ref())
                        }) {
                            append_local_command_invocation_result(
                                &mut history_state,
                                invocation,
                                &message,
                                false,
                            );
                        } else {
                            push_system_notice(
                                &mut history_state,
                                SystemMessageLevel::Warning,
                                message,
                            );
                        }
                    }
                }
                user_input_on_processing.set(None);
                if let Some(picker) = picker {
                    // CC resume.tsx:230-231: onDone(undefined, display:skip)
                    // follows the actual state commit, never the selection.
                    if resume_succeeded { let _ = picker.completion.set(()); }
                    if let Some(mut current) = active_local_command_ui.try_write() {
                        if current.as_ref().is_some_and(|current| Arc::ptr_eq(&current.completion, &picker.completion)) {
                            *current = None;
                        }
                    }
                } else {
                    active_local_command_ui.set(clear_local_command_ui(active));
                }
            }
        }
    });
    // CC handleRestoreMessage: defer the synchronous rewind until the selector
    // has closed, matching setImmediate through the retained async handler.
    let handle_restore_message = hooks.use_async_handler({
        let store = app_store.clone();
        move |row_uuid: String| {
            let store = store.clone();
            async move {
                futures_timer::Delay::new(Duration::ZERO).await;
                if let Some(message) =
                    rewind_conversation_to(history_state, conversation_id, &store, &row_uuid)
                {
                    let revision = restored_input
                        .read()
                        .as_ref()
                        .map_or(1, |update| update.revision.wrapping_add(1));
                    let mut update =
                        restore_message_sync(&message, revision, &prompt_input_snapshot.read());
                    // CC leaves mode/images untouched when the matching setter
                    // does not run. Preserve that state across selector unmounts.
                    if let Some(previous) = restored_input.read().as_ref() {
                        if update.mode.is_none() {
                            update.mode = previous.mode;
                            update.cursor_offset = previous.cursor_offset;
                        }
                        if update.pasted_contents.is_none() {
                            update.pasted_contents = previous.pasted_contents.clone();
                        }
                    }
                    if crate::utils::messages::get_user_message_text(&message).is_some() {
                        set_input_value(update.text.clone());
                    }
                    restored_input.set(Some(update));
                }
            }
        }
    });
    // Maps to CC REPL.tsx:6893-7019 onSummarize. The retained handler owns
    // history/input writes; the existing per-query Tokio worker owns model I/O.
    let handle_summarize = hooks.use_async_handler({
        let store = app_store.clone();
        let commands = commands.clone();
        let initial_tools = initial_tools.clone();
        let overrides = system_prompt_overrides.clone();
        let permission_sink = interactive_permission_sink.clone();
        let thinking = thinking_config.clone();
        move |(uuid, feedback, direction, completion): (
            String, Option<String>, crate::types::message::PartialCompactDirection,
            futures::channel::oneshot::Sender<Result<(), String>>,
        )| {
            let store = store.clone();
            let messages = crate::utils::messages::get_messages_after_compact_boundary(
                &history_model_messages(&history_state.read()));
            let index = messages.iter().position(|message| message.uuid() == uuid);
            let state = store.get();
            let agent = main_thread_agent_definition.read().clone();
            let context = build_repl_process_user_input_context(
                (*state.tool_permission_context).clone(), &store, messages.clone(),
                resume_restore_stores.read().clone(), loaded_nested_memory_paths.read().clone(),
                (*state.mcp).clone(), &initial_tools, commands.clone(), debug, &overrides,
                agent.as_deref(), state.channel_permission_callbacks.as_ref(), &permission_sink,
                &thinking, repl_response_length_sink(response_length_ref),
            );
            let overrides = overrides.clone();
            async move {
                use crate::types::message::PartialCompactDirection;
                let Some(index) = index else {
                    push_system_notice(
                        &mut history_state,
                        SystemMessageLevel::Warning,
                        "That message is no longer in the active context (snipped or pre-compact). Choose a more recent message.",
                    );
                    let _ = completion.send(Ok(()));
                    return;
                };
                let selected = messages[index].clone();
                active_compact_abort.set(Some(context.abort_controller.clone()));
                stream_mode.set(SpinnerMode::Requesting);
                response_length_ref.set(0);
                let (sender, receiver) = futures::channel::oneshot::channel();
                let spawned = std::thread::Builder::new().name("cometix-partial-compact".into())
                    .stack_size(8 * 1024 * 1024).spawn(move || {
                        let result = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                            Ok(runtime) => runtime.block_on(async {
                                let app_state = context.get_app_state().expect("REPL compact context has an AppStore");
                                let model = context.main_loop_model.clone().unwrap_or_else(crate::utils::model::model::get_main_loop_model);
                                let directories = app_state.tool_permission_context.additional_working_directories.keys().cloned().collect::<Vec<_>>();
                                let default_prompt = crate::constants::prompts::get_system_prompt(&context.tools, &model, &directories, &context.mcp_state.clients);
                                let system_prompt = overrides.apply_with_agent(default_prompt, None, None, None);
                                let cache = crate::services::compact::auto_compact::AutoCompactCacheSafeParams {
                                    system_prompt, user_context: crate::context::get_user_context(),
                                    system_context: crate::context::get_system_context(), fork_context_messages: messages.clone(),
                                };
                                crate::services::compact::compact::partial_compact_conversation(
                                    messages, index, &context, &cache, feedback.as_deref(), direction,
                                ).await
                            }),
                            Err(error) => Err(format!("Failed to start compact runtime: {error}")),
                        };
                        let _ = sender.send(result);
                    });
                let result = match spawned {
                    Ok(_) => receiver.await.unwrap_or_else(|error| Err(error.to_string())),
                    Err(error) => Err(format!("Failed to start compact worker: {error}")),
                };
                active_compact_abort.set(None);
                stream_mode.set(SpinnerMode::Requesting);
                response_length_ref.set(0);
                let result = result.map(|result| {
                    let mut restored = resume_restore_stores.read().clone();
                    restored.read_file_state = result.rebuilt_read_file_state.clone();
                    resume_restore_stores.set(restored);
                    loaded_nested_memory_paths.set(std::collections::HashSet::new());
                    let kept = result.messages_to_keep.unwrap_or_default();
                    let summaries = result.summary_messages.into_iter().map(Message::User);
                    let mut post = vec![Message::System(result.boundary_marker)];
                    if direction == PartialCompactDirection::UpTo { post.extend(summaries); post.extend(kept); }
                    else { post.extend(kept); post.extend(summaries); }
                    post.extend(result.attachments.into_iter().map(Message::Attachment));
                    post.extend(result.hook_results);
                    let mut entries = if crate::utils::fullscreen::is_fullscreen_env_enabled() && direction == PartialCompactDirection::From {
                        let previous = history_state.read();
                        let index = previous.iter().position(|entry| entry.uuid() == uuid).unwrap_or(0);
                        previous[..index].to_vec()
                    } else { Vec::new() };
                    entries.extend(post.into_iter().map(HistoryEntry::Message));
                    history_state.set(Arc::new(entries));
                    conversation_id.set(conversation_id.get().wrapping_add(1));
                    crate::services::compact::post_compact_cleanup::run_post_compact_cleanup(None);
                    if direction == PartialCompactDirection::From {
                        if let Message::User(message) = selected {
                            if let Some((text, mode)) = crate::utils::messages::text_for_resubmit(&message) {
                                let revision = restored_input.read().as_ref().map_or(1, |update| update.revision.wrapping_add(1));
                                let pasted_contents = restored_input.read().as_ref().and_then(|update| update.pasted_contents.clone());
                                restored_input.set(Some(crate::components::prompt_input::PromptInputTextUpdate { revision, cursor_offset: Some(text.len()), text: text.clone(), mode: Some(mode), pasted_contents }));
                                set_input_value(text);
                            }
                        }
                    }
                    let shortcut = crate::keybindings::shortcut_format::get_shortcut_display("app:toggleTranscript", &crate::keybindings::types::ContextName::Global, "ctrl+o");
                    crate::context::notifications::NotificationsWriter::new(store.clone()).add_notification(
                        crate::context::notifications::Notification::text("summarize-ctrl-o-hint", format!("Conversation summarized ({shortcut} for history)"), crate::context::notifications::NotificationPriority::Medium).with_timeout_ms(8000));
                });
                if let Err(error) = &result {
                    if error != crate::services::compact::compact::ERROR_MESSAGE_USER_ABORT && error != crate::services::compact::compact::ERROR_MESSAGE_NOT_ENOUGH_MESSAGES {
                        crate::context::notifications::NotificationsWriter::new(store).add_notification(
                            crate::context::notifications::Notification::text("error-compacting-conversation", "Error compacting conversation", crate::context::notifications::NotificationPriority::Immediate).with_color(crate::context::notifications::NotificationColor::Error));
                    }
                }
                let _ = completion.send(result);
            }
        }
    });
    let mut prompt_modal_overlay_active = hooks.use_state(|| false);
    // Maps to: CC REPL.tsx:1408-1409 `const [autoUpdaterResult,
    // setAutoUpdaterResult] = useState<AutoUpdaterResult | null>(null)`.
    // The REPL.tsx:1411-1421 addNotification fan-out runs inline in the
    // `on_auto_updater_result` setter handler below.
    let auto_updater_result =
        hooks.use_state(|| Option::<crate::utils::auto_updater::AutoUpdaterResult>::None);
    // Maps to: CC `useAppState(s => s.verbose)` — AppState is the sole authority
    // (Config preview and /verbose write the store; footer indicators overlay
    // the same value when rendering).
    let verbose = crate::state::app_state::use_app_state(&mut hooks, |state| state.verbose);
    let mock_pump_channel =
        hooks.use_const(|| std::sync::Arc::new(async_channel::unbounded::<()>()));
    let mock_pump_tx = mock_pump_channel.0.clone();
    let mock_pump_rx = mock_pump_channel.1.clone();
    let mut active_prompt_shell_command = hooks.use_state(|| Option::<String>::None);
    let prompt_shell_channel = hooks.use_const(|| {
        std::sync::Arc::new(async_channel::unbounded::<(
            String,
            Result<crate::tools::bash_tool::BashOutput, String>,
        )>())
    });
    let prompt_shell_receiver = prompt_shell_channel.1.clone();
    hooks.use_future(async move {
        while let Ok((command, result)) = prompt_shell_receiver.recv().await {
            // The message shapes belong to CC's `processBashCommand.tsx`; only
            // the execution half stays here, because the iocraft submit
            // handler cannot await the way CC's `processUserInput` does.
            use crate::utils::process_user_input::process_bash_command::{
                BashCommandOutcome, process_bash_command,
            };
            let outcome = match result {
                Ok(output) if output.interrupted => BashCommandOutcome::Interrupted,
                Ok(output) => BashCommandOutcome::Completed {
                    stdout: output.stdout,
                    stderr: output.stderr,
                },
                Err(message) => BashCommandOutcome::Failed { message },
            };
            // C3c-4: one messages array serves both — the user rows become
            // `Message` entries, so the model projection sees them without a
            // separate typed-history write (CC's single-array shape).
            let processed = process_bash_command(&command, outcome, Vec::new());
            append_history_rows(&mut history_state, processed.messages);
            active_prompt_shell_command.set(None);
        }
    });
    crate::hooks::use_global_keybindings::use_global_keybindings(
        &mut hooks,
        keybinding_runtime_for_reload.clone(),
        app_store.clone(),
        redraw_generation,
        screen,
        show_all_in_transcript,
    );

    let active_query_for_on_cancel = active_query;
    let active_compact_for_on_cancel = active_compact_abort;
    let permission_queue_for_cancel = permission_queue;
    let history_state_for_cancel = history_state;
    let streaming_preview_for_cancel = streaming_text_preview;
    let on_cancel = move || {
        // Maps to: CC handleCancel priority 1 —
        // setToolUseConfirmQueue([]) + onCancel() →
        // abortController.abort('user-cancel').
        let mut permission_queue_for_cancel = permission_queue_for_cancel;
        permission_queue_for_cancel.set(Vec::new());
        if let Some(handle) = active_query_for_on_cancel.read().clone() {
            // Maps to: CC REPL.tsx:2839-2844 — preserve partial
            // query text before aborting. Compact has no assistant
            // preview row and therefore skips this query-only path.
            let mut history_state_for_cancel = history_state_for_cancel;
            let mut streaming_preview_for_cancel = streaming_preview_for_cancel;
            append_partial_streaming_text_message(
                &mut history_state_for_cancel,
                streaming_preview_for_cancel.read().clone(),
            );
            streaming_preview_for_cancel.set(None);
            // No interrupt row here. CC's onCancel
            // (REPL.tsx:2836-2843) pushes ONLY the partial
            // assistant, and says why: it lands "before query.ts
            // yields the async interrupt marker, giving final
            // order [user, partial-assistant, [Request interrupted
            // by user]]". The marker is query's
            // (`query.ts:1047,1502`), which is also where CC's
            // `signal.reason !== 'interrupt'` guard and the
            // toolUse wording live — minting it here bypassed
            // both.
            let _ = handle.commands.try_send(QueryCommand::Abort);
            handle.abort_controller.abort();
        }
        if let Some(abort) = active_compact_for_on_cancel.read().clone() {
            abort.abort();
        }
    };

    // Maps to: CC REPL.tsx:5891 mounting `<CancelRequestHandler {...props}/>`
    // inside KeybindingSetup; the handler itself lives in
    // hooks/use_cancel_request.rs (CC hooks/useCancelRequest.ts).
    {
        let active_query_for_cancel = active_query;
        let active_compact_for_cancel = active_compact_abort;
        let is_context_blocked = {
            // Maps to: CC isContextActive guards (:141-148). Messages
            // screen and local command panels own their own Escape.
            let showing_local_command_ui = active_local_command_ui.read().is_some();
            showing_local_command_ui || screen.get() == Screen::Transcript
        };
        crate::hooks::use_cancel_request::use_cancel_request(
            &mut hooks,
            crate::hooks::use_cancel_request::UseCancelRequestOptions {
                app_store: app_store.clone(),
                can_cancel_running_task: move || {
                    active_query_for_cancel
                        .read()
                        .as_ref()
                        .is_some_and(|handle| !handle.abort_controller.is_aborted())
                        || active_compact_for_cancel
                            .read()
                            .as_ref()
                            .is_some_and(|abort| !abort.is_aborted())
                },
                on_cancel,
                is_context_blocked,
            },
        );
    }

    hooks.use_future({
        let runtime_mcp_context = runtime_mcp_context.clone();
        let has_interruptible_tool_in_progress =
            Arc::clone(&has_interruptible_tool_in_progress);
        let mut history_state = history_state;
        let mut conversation_id = conversation_id;
        let mut content_replacement_state = content_replacement_state;
        let mut pending_responses = pending_responses;
        let mut streaming_text_preview = streaming_text_preview;
        let mut stream_mode = stream_mode;
        let mut response_length_ref = response_length_ref;
        let mut stop_hook_spinner_state = stop_hook_spinner_state;
        let mut active_query = active_query;
        let mut active_local_command_ui = active_local_command_ui;
        let mut permission_queue = permission_queue;
        let mut classifier_approvals = classifier_approvals;
        let permission_store = permission_store.clone();
        let mut resume_restore_stores = resume_restore_stores;
        let mut loaded_nested_memory_paths = loaded_nested_memory_paths;
        let mock_pump_rx = mock_pump_rx.clone();
        let query_pump_profile = crate::utils::debug::query_pump_profile_enabled();
        async move {
            let mut query_pump_burst = QueryPumpBurstProfile::default();
            loop {
                if !permission_queue.read().is_empty() {
                    // Mirrors CC query/canUseTool gating: once a permission
                    // dialog is visible, no later mock query messages are
                    // rendered until REPL receives an approval/cancel signal.
                    if mock_pump_rx.recv().await.is_err() {
                        break;
                    }
                    continue;
                }

                let active_handle = { active_query.read().clone() };
                if let Some(handle) = active_handle {
                    let event = handle.events.recv().await;
                    let still_active = {
                        active_query
                            .read()
                            .as_ref()
                            .map(|current| current.id == handle.id.as_str())
                            .unwrap_or(false)
                    };
                    if !still_active {
                        continue;
                    }
                    if query_pump_profile {
                        if let Ok(event) = &event {
                            query_pump_burst.record_event(event);
                        }
                    }
                    match event {
                        Ok(QueryEvent::StreamRequestStart) => {
                            stream_mode.set(SpinnerMode::Requesting);
                            response_length_ref.set(0);
                            stop_hook_spinner_state.set(StopHookSpinnerState::default());
                        }
                        Ok(QueryEvent::Stream(event)) => {
                            // Maps to CC `utils/messages.ts#handleMessageFromStream`:
                            // stream events update `streamMode`, `responseLengthRef`,
                            // and REPL-local `streamingText` without appending formal rows.
                            if stream_event_type(&event) == Some("message_stop") {
                                stream_mode.set(SpinnerMode::ToolUse);
                            } else if stream_event_type(&event) == Some("message_delta") {
                                stream_mode.set(SpinnerMode::Responding);
                            } else if let Some(block_type) = stream_event_content_block_start_type(&event) {
                                streaming_text_preview.set(None);
                                stream_mode.set(stream_block_type_to_spinner_mode(block_type));
                            }

                            if let Some(delta) = stream_event_delta_for_response_length(&event) {
                                // JS `String.length` counts UTF-16 code units;
                                // match it for spinner token estimates.
                                response_length_ref += delta.encode_utf16().count();
                            }
                            if let Some(delta) = stream_event_text_delta(&event) {
                                let mut next =
                                    streaming_text_preview.read().clone().unwrap_or_default();
                                next.raw.push_str(&delta);
                                streaming_text_preview.set(Some(next));
                            }
                        }
                        Ok(QueryEvent::ClearStreamingPreview) => {
                            streaming_text_preview.set(None);
                        }
                        Ok(QueryEvent::ToolUseSummary(_summary)) => {}
                        Ok(QueryEvent::Message(message)) => {
                            // C3c-4: the seam yields a whole model `Message`
                            // (CC query.ts yields Message values) and it
                            // enters the ONE history — render rows and API
                            // history are projections of the same entry.
                            streaming_text_preview.set(None);
                            // CC REPL.tsx:3440-3526: producers update history;
                            // useLogMessages owns transcript parent selection and writes.
                            if crate::utils::messages::is_compact_boundary_message(&message) {
                                // CC REPL.tsx:3443-3463 (non-fullscreen):
                                // `setMessages(() => [newMessage])` — the
                                // history resets to the boundary; the rest of
                                // the post-compact flood (query.ts:528-535)
                                // appends behind it via the ordinary branch
                                // below. Pre-compact rows live on in native
                                // scrollback (task #8), and history regains
                                // CC's O(n) per-render cap: one compact
                                // interval. Then `setConversationId(
                                // randomUUID())` (REPL.tsx:3461-3463) so
                                // stale memoized rows remount.
                                history_state
                                    .set(Arc::new(vec![HistoryEntry::Message(message)]));
                                conversation_id += 1;
                            } else {
                                let mut entries = history_state.read().as_ref().clone();
                                push_or_replace_entry(
                                    &mut entries,
                                    HistoryEntry::Message(message),
                                );
                                history_state.set(Arc::new(entries));
                            }
                        }
                        Ok(QueryEvent::AssistantDelta {
                            uuid,
                            stop_reason,
                            usage,
                        }) => {
                            // CC claude.ts:2229-2248: `message_delta` mutates
                            // the last yielded per-block assistant through its
                            // shared reference; the appended history entry and
                            // the lazily-serialized transcript record both see
                            // the final usage/stop_reason. Rust applies the
                            // same mutation by uuid: the history entry in
                            // place, then the queued entry before its 100 ms drain.
                            let mut entries = history_state.read().as_ref().clone();
                            if let Some(HistoryEntry::Message(Message::Assistant(assistant))) =
                                entries
                                    .iter_mut()
                                    .rev()
                                    .find(|entry| entry.uuid() == uuid.as_str())
                            {
                                assistant.stop_reason = stop_reason.clone();
                                assistant.usage = usage.clone();
                                history_state.set(Arc::new(entries));
                            }
                            if let Err(error) = crate::utils::session_storage::apply_assistant_delta_to_queued_record(&uuid,
                            stop_reason,
                            usage,)
                            {
                                crate::utils::debug::log_for_debugging(&format!(
                                    "Failed to record assistant delta: {error}"
                                ));
                            }
                        }
                        Ok(QueryEvent::Row(message)) => {
                            // Render-only row (batch-D kinds and dual-carrier
                            // render halves); history is fed by the paired
                            // ModelMessage arm.
                            streaming_text_preview.set(None);
                            let mut entries = history_state.read().as_ref().clone();
                            push_or_replace_entry(&mut entries, HistoryEntry::Row(message));
                            history_state.set(Arc::new(entries));
                        }
                        // Maps to CC `REPL.tsx:3910` `setStreamingToolUses`.
                        Ok(QueryEvent::StreamingToolUseStarted { tool_use_id }) => {
                            let mut ids = streaming_tool_use_ids.read().as_ref().clone();
                            if ids.insert(tool_use_id) {
                                streaming_tool_use_ids.set(Arc::new(ids));
                            }
                        }
                        // Maps to CC `Tool.ts:227` `setInProgressToolUseIDs(prev => …)`
                        // applied to the REPL-owned set (`REPL.tsx:1897`).
                        Ok(QueryEvent::SetInProgressToolUse {
                            tool_use_id,
                            in_progress,
                        }) => {
                            let mut ids = in_progress_tool_use_ids.read().as_ref().clone();
                            let changed = if in_progress {
                                ids.insert(tool_use_id)
                            } else {
                                ids.remove(&tool_use_id)
                            };
                            if changed {
                                in_progress_tool_use_ids.set(Arc::new(ids));
                            }
                        }
                        Ok(QueryEvent::Tombstone(tombstone)) => {
                            let tombstone_uuid = tombstone.message.uuid().to_string();
                            let mut entries = history_state.read().as_ref().clone();
                            remove_tombstoned_entry(&mut entries, &tombstone_uuid);
                            history_state.set(Arc::new(entries));
                            // Maps to CC `REPL.tsx:3521-3525` tombstone
                            // callback: `void removeTranscriptMessage(
                            // tombstonedMessage.uuid)`.
                            let removal =
                                crate::utils::session_storage::remove_transcript_message(
                                    &tombstone_uuid,
                                );
                            tokio::spawn(async move {
                                if let Err(error) = removal.await {
                                    crate::utils::debug::log_for_debugging(&format!(
                                        "Failed to remove tombstoned transcript message: {error}"
                                    ));
                                }
                            });
                        }
                        Ok(QueryEvent::ToolProgress(
                            crate::types::tools::ToolProgress::BashProgress {
                                tool_use_id: progress_tool_use_id,
                                output,
                                full_output,
                                elapsed_time_seconds,
                                total_lines,
                                total_bytes,
                                task_id,
                                timeout_ms,
                            },
                        )) => {
                            update_tool_use_progress_message(
                                &mut history_state,
                                &progress_tool_use_id.0,
                                crate::types::message::ToolUseProgressMessage::BashProgress {
                                    output,
                                    full_output,
                                    elapsed_time_seconds,
                                    total_lines,
                                    total_bytes,
                                    task_id,
                                    timeout_ms,
                                },
                            );
                        }
                        Ok(QueryEvent::ToolProgress(
                            crate::types::tools::ToolProgress::WebSearchQueryUpdate {
                                tool_use_id: progress_tool_use_id,
                                query,
                            },
                        )) => {
                            update_tool_use_progress_message(
                                &mut history_state,
                                &progress_tool_use_id.0,
                                crate::types::message::ToolUseProgressMessage::QueryUpdate {
                                    query,
                                },
                            );
                        }
                        Ok(QueryEvent::ToolProgress(
                            crate::types::tools::ToolProgress::WebSearchResultsReceived {
                                tool_use_id: progress_tool_use_id,
                                query,
                                result_count,
                            },
                        )) => {
                            update_tool_use_progress_message(
                                &mut history_state,
                                &progress_tool_use_id.0,
                                crate::types::message::ToolUseProgressMessage::SearchResultsReceived {
                                    query,
                                    result_count,
                                },
                            );
                        }
                        Ok(QueryEvent::ToolProgress(
                            crate::types::tools::ToolProgress::AgentProgress {
                                parent_tool_use_id: progress_tool_use_id,
                                message,
                                prompt,
                                agent_id,
                            },
                        )) => {
                            if let Some(message) = subagent_progress_render_message(*message) {
                                update_tool_use_progress_message(
                                    &mut history_state,
                                    &progress_tool_use_id.0,
                                    crate::types::message::ToolUseProgressMessage::AgentProgress {
                                        message: Box::new(message),
                                        prompt,
                                        agent_id,
                                    },
                                );
                            }
                        }
                        Ok(QueryEvent::ToolProgress(
                            crate::types::tools::ToolProgress::SkillProgress {
                                parent_tool_use_id: progress_tool_use_id,
                                message,
                                prompt,
                                agent_id,
                            },
                        )) => {
                            if let Some(message) = subagent_progress_render_message(*message) {
                                update_tool_use_progress_message(
                                    &mut history_state,
                                    &progress_tool_use_id.0,
                                    crate::types::message::ToolUseProgressMessage::SkillProgress {
                                        message: Box::new(message),
                                        prompt,
                                        agent_id,
                                    },
                                );
                            }
                        }
                        Ok(QueryEvent::ToolProgress(
                            crate::types::tools::ToolProgress::TaskOutputWaiting {
                                tool_use_id: progress_tool_use_id,
                                task_description,
                                task_type,
                            },
                        )) => {
                            update_tool_use_progress_message(
                                &mut history_state,
                                &progress_tool_use_id.0,
                                crate::types::message::ToolUseProgressMessage::WaitingForTask {
                                    task_description,
                                    task_type,
                                },
                            );
                        }
                        Ok(QueryEvent::ToolProgress(_)) => {}
                        Ok(QueryEvent::StopHookProgress(progress)) => {
                            let mut state = stop_hook_spinner_state.read().clone();
                            state.apply(progress);
                            stop_hook_spinner_state.set(state);
                        }
                        Ok(QueryEvent::ModelMessage(message)) => {
                            // History-only half of the dual carrier: its
                            // render half arrived as `Row` entries, so it
                            // enters the one history as `ModelOnly` (the
                            // render projection must not draw it twice).
                            let mut entries = history_state.read().as_ref().clone();
                            entries.push(HistoryEntry::ModelOnly(message));
                            history_state.set(Arc::new(entries));
                        }
                        Ok(QueryEvent::ApiError(_)) => {
                            // The paired QueryEvent::Message owns rendering;
                            // retain this typed payload for forked consumers.
                        }
                        Ok(QueryEvent::ContentReplacementStateUpdate(state)) => {
                            content_replacement_state.set(state);
                        }
                        Ok(QueryEvent::PermissionRequest(request)) => {
                            // Maps to: CC `useCanUseTool.tsx:70,307-324` — the
                            // queued entry closes over the `resolve` of the
                            // promise `canUseTool` returned to ITS caller, and
                            // `interactiveHandler.ts:204-231` gives that same
                            // entry a `recheckPermission`. This row's promise
                            // lives in the query actor below, so its resolver
                            // is `QueryCommand::PermissionResponse` addressed
                            // at THIS handle — not at whichever query
                            // `active_query` names when the answer arrives, and
                            // not nothing at all, which is what left the sweep
                            // unable to settle the leader's own prompts.
                            let commands = handle.commands.clone();
                            let tool_use_id = request.tool_use_id.clone();
                            let confirm = tool_use_confirm_for_request(
                                request,
                                crate::types::permissions::PermissionPromptResponder::from_command_sink(
                                    move |response| {
                                        commands
                                            .try_send(QueryCommand::PermissionResponse {
                                                tool_use_id: tool_use_id.clone(),
                                                response,
                                            })
                                            .is_ok()
                                    },
                                ),
                                // CC `interactiveHandler.ts:97` `toolUseContext:
                                // ctx.toolUseContext`. For the REPL's OWN query
                                // that context is the leader's, whose
                                // `getAppState()` is the untransformed store
                                // read — so the row records no override and the
                                // recheck sweep re-evaluates it against the live
                                // context, which IS its asking context.
                                None,
                            );
                            let mut queue = permission_queue.read().clone();
                            crate::hooks::tool_permission::permission_context::push_to_queue(
                                &mut queue, confirm,
                            );
                            permission_queue.set(queue);
                        }
                        Ok(QueryEvent::PermissionContextUpdate(context)) => {
                            permission_store.set_tool_permission_context(context);
                        }
                        Ok(QueryEvent::ToolContextUpdate(context)) => {
                            // ToolUseContext carries mutable Read/Bash caches in
                            // CC. Rust transports those effects out of the query
                            // actor so the next top-level interactive turn sees
                            // post-tool and post-compact state.
                            let mut stores = resume_restore_stores.read().clone();
                            stores.read_file_state = context.read_file_state.snapshot();
                            stores.bash_tools = context.bash_tools.clone();
                            resume_restore_stores.set(stores);
                            loaded_nested_memory_paths
                                .set(context.loaded_nested_memory_paths.clone());
                        }
                        Ok(QueryEvent::StructuredOutput(_)) => {
                            // StructuredOutput is SDK/headless-only.
                        }
                        Ok(QueryEvent::It2SetupPromptRequest(request)) => {
                            active_local_command_ui
                                .set(Some(ActiveLocalCommandUi::from_it2_setup_request(request)));
                        }
                        Ok(QueryEvent::Terminal(_)) | Err(_) => {

                            // Maps to CC `QueryEngine.query()` eager/cowork
                            // result boundary.
                            if crate::utils::env_utils::is_env_truthy(std::env::var(
                                "CLAUDE_CODE_EAGER_FLUSH").ok().as_deref()) || crate::utils::env_utils::is_env_truthy(std::env::var(
                                "CLAUDE_CODE_IS_COWORK").ok().as_deref()) {
                                if let Err(error) =
                                    crate::utils::session_storage::flush_session_storage().await
                                {
                                    crate::utils::debug::log_for_debugging(&format!(
                                        "Failed to flush session storage at query completion: {error}"
                                    ));
                                }
                            }
                            streaming_text_preview.set(None);
                            has_interruptible_tool_in_progress
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                            // Maps to CC `REPL.tsx:2118,3910` `setStreamingToolUses([])`,
                            // cleared alongside `setStreamingText(null)` at the
                            // same turn boundary.
                            if !streaming_tool_use_ids.read().is_empty() {
                                streaming_tool_use_ids
                                    .set(Arc::new(std::collections::HashSet::new()));
                            }
                            stop_hook_spinner_state.set(StopHookSpinnerState::default());
                            active_query.set(None);
                            // Maps to: CC `resetLoadingState` → `pickNewSpinnerTip`.
                            crate::services::tips::pick_new_spinner_tip(&permission_store);
                        }
                    }
                    if query_pump_profile {
                        let queue_len = handle.events.len();
                        query_pump_burst.maybe_log_and_reset(queue_len, queue_len == 0);
                    }
                    continue;
                }

                if pending_responses.read().is_empty() && mock_pump_rx.recv().await.is_err() {
                    break;
                }

                loop {
                    if !permission_queue.read().is_empty() {
                        break;
                    }

                    let pending_snapshot = pending_responses.read().clone();
                    if pending_snapshot.is_empty() {
                        break;
                    }

                    if let Some(next_due) =
                        pending_snapshot.iter().map(|pending| pending.due_at).min()
                    {
                        let now = Instant::now();
                        if next_due > now {
                            futures_timer::Delay::new(next_due - now).await;
                        }
                    }

                    let now = Instant::now();
                    let pending_snapshot = pending_responses.read().clone();
                    let mut waiting = Vec::new();
                    let mut entries = history_state.read().as_ref().clone();
                    let mut permissions = permission_queue.read().clone();
                    let mut blocked_on_permission = false;
                    let mut made_progress = false;

                    for pending in pending_snapshot {
                        if blocked_on_permission || pending.due_at > now {
                            waiting.push(pending);
                            continue;
                        }

                        made_progress = true;
                        let restore_store_snapshot = resume_restore_stores.read().clone();
                        let mut tool_use_context = ToolUseContext::with_permission_context(
                            permission_store.tool_permission_context(),
                        )
                        .with_app_store(permission_store.clone())
                        .with_resume_restore_stores(restore_store_snapshot)
                        .with_mcp_state(
                            runtime_mcp_context
                                .as_ref()
                                .map(|context| context.current())
                                .unwrap_or_default(),
                        );
                        tool_use_context.loaded_nested_memory_paths =
                            loaded_nested_memory_paths.read().clone();
                        let update = run_tools_for_message(
                            &pending.message,
                            &tool_use_context,
                            &mut permissions,
                        );
                        if let Some(classifier_state) = pending.classifier_checking.clone() {
                            classifier_approvals
                                .set(ClassifierApprovalsState::start_checking(classifier_state));
                        }
                        // Mock-runtime rows stay render-only `Row` entries:
                        // the scripted pending conversation never fed typed
                        // history before the cutover either.
                        entries.push(HistoryEntry::Row(pending.message));
                        if update.blocked_on_permission {
                            blocked_on_permission = true;
                        }
                    }

                    if !made_progress {
                        continue;
                    }

                    pending_responses.set(waiting);
                    permission_queue.set(permissions);
                    history_state.set(Arc::new(entries));

                    if blocked_on_permission {
                        break;
                    }
                }
            }
        }
    });

    // Shift+Tab calls the Rust counterpart of CC
    // `utils/permissions/getNextPermissionMode.ts` `cyclePermissionMode(...)`.
    // REPL owns the live permission context and PromptInput only reports the
    // intent.
    let permission_context_for_mode_cycle = permission_store.clone();
    let on_permission_mode_cycle = move |_: ()| {
        let (next_mode, mut next_context) =
            crate::utils::permissions::get_next_permission_mode::cycle_permission_mode(
                &permission_context_for_mode_cycle.tool_permission_context(),
            );
        // Maps to: CC PromptInput.tsx:1969-1996: the caller installs the
        // selected mode after cyclePermissionMode prepares the context.
        next_context.mode = next_mode;
        permission_context_for_mode_cycle.set_tool_permission_context(next_context);
    };

    // Maps to: CC `processPromptSlashCommand(...)` for dynamic MCP prompt
    // commands returned by `services/mcp/client.ts#fetchCommandsForClient`.
    // iocraft input handlers are synchronous, so REPL drives the official async
    // `getPromptForCommand(args)` step with a component-bound async handler and
    // then rejoins the normal handlePromptSubmit/query path.
    let mcp_prompt_submit_action = hooks.use_async_handler({
        let runtime_mcp_context = runtime_mcp_context.clone();
        let prompt_submit_for_mcp = prompt_submit.clone();
        let system_prompt_overrides = system_prompt_overrides.clone();
        let mut history_state = history_state;
        let mut pending_responses = pending_responses;
        let mut content_replacement_state = content_replacement_state;
        let resume_restore_stores = resume_restore_stores;
        let loaded_nested_memory_paths = loaded_nested_memory_paths;
        let main_thread_agent_definition = main_thread_agent_definition;
        let mut active_query = active_query;
        let mut user_input_on_processing = user_input_on_processing;
        let mut stream_mode = stream_mode;
        let mut response_length_ref = response_length_ref;
        let mock_pump_tx_for_mcp = mock_pump_tx.clone();
        let channel_permission_callbacks_for_mcp = channel_permission_callbacks.clone();
        let permission_sink_for_mcp = interactive_permission_sink.clone();
        let app_store_for_mcp = app_store.clone();
        let thinking_config_for_mcp = thinking_config.clone();
        let initial_tools_for_mcp = initial_tools.clone();
        let has_interruptible_tool_in_progress =
            Arc::clone(&has_interruptible_tool_in_progress);
        move |submit: McpPromptSlashCommandSubmit| {
            let runtime_mcp_context = runtime_mcp_context.clone();
            let prompt_submit_for_mcp = prompt_submit_for_mcp.clone();
            let mock_pump_tx_for_mcp = mock_pump_tx_for_mcp.clone();
            let channel_permission_callbacks_for_mcp =
                channel_permission_callbacks_for_mcp.clone();
            let permission_sink_for_mcp = permission_sink_for_mcp.clone();
            let app_store_for_mcp = app_store_for_mcp.clone();
            let system_prompt_overrides = system_prompt_overrides.clone();
            let thinking_config_for_mcp = thinking_config_for_mcp.clone();
            let initial_tools_for_mcp = initial_tools_for_mcp.clone();
            let has_interruptible_tool_in_progress =
                Arc::clone(&has_interruptible_tool_in_progress);
            async move {
                let blocks = match crate::services::mcp::client::get_mcp_prompt_for_command(
                &submit.server_name,
                &submit.prompt.name,
                &submit.prompt.arg_names,
                &submit.args,
            )
            .await
            {
                Ok(blocks) => blocks,
                Err(error) => {
                    // Maps to CC `processSlashCommand.tsx` prompt-command catch:
                    // append command input plus local-command stderr and do not
                    // query the model.
                    let processed = crate::utils::process_user_input::process_slash_command::mcp_prompt_slash_command_error_result(
                        Some(submit.turn_id.clone()),
                        &submit.command,
                        &submit.args,
                        &error.to_string(),
                    );
                    apply_repl_command_allowed_tools(&app_store_for_mcp, &[]);
                    append_history_rows(&mut history_state, processed.messages);
                    user_input_on_processing.set(None);
                    let _ = mock_pump_tx_for_mcp.try_send(());
                    return;
                }
            };

            let processed = crate::utils::process_user_input::process_slash_command::mcp_prompt_slash_command_result(
                Some(submit.turn_id.clone()),
                &submit.command,
                &submit.args,
                &blocks,
            );
            let result = prompt_submit_for_mcp.submit_processed_prompt_deferred_query(
                submit.input.clone(),
                submit.turn_id.clone(),
                processed,
                submit.tool_permission_context,
            );
            let command_permission_context =
                apply_repl_command_allowed_tools(&app_store_for_mcp, &result.allowed_tools);
            let Some(mut params) = result.query_params.clone() else {
                append_prompt_submit_result(&mut history_state, &mut pending_responses, result);
                user_input_on_processing.set(None);
                let _ = mock_pump_tx_for_mcp.try_send(());
                return;
            };

            if has_interruptible_tool_in_progress
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                if let Some(handle) = active_query.read().clone() {
                    handle.abort_controller.abort_with_reason("interrupt");
                    let _ = handle.commands.try_send(QueryCommand::Abort);
                }
            }
            // C3c-4: submitted user rows enter the ONE history as `Message`
            // entries (model-visibility by type); the render rows and the
            // typed API history below are projections of the same entries.
            append_prompt_submit_result(&mut history_state, &mut pending_responses, result);
            let entries_snapshot = Arc::clone(&*history_state.read());
            let main_thread_agent_definition = main_thread_agent_definition.read().clone();
            // Maps to: CC REPL.tsx:3703-3714, both checks settle before the query starts.
            let gate_snapshot = app_store_for_mcp.get();
            tokio::join!(
                crate::utils::permissions::bypass_permissions_killswitch::check_and_disable_bypass_permissions_if_needed(
                    &command_permission_context, &app_store_for_mcp),
                crate::utils::permissions::bypass_permissions_killswitch::check_and_disable_auto_mode_if_needed(
                    &command_permission_context, &app_store_for_mcp, Some(gate_snapshot.fast_mode)),
            );
            let next_content_replacement_state = apply_repl_query_turn_context(
                &mut params,
                &app_store_for_mcp,
                command_permission_context,
                &entries_snapshot,
                content_replacement_state.read().clone(),
                &resume_restore_stores.read(),
                runtime_mcp_context.as_ref().map(|context| context.current()),
                main_thread_agent_definition.as_deref(),
                initial_tools_for_mcp.as_slice(),
                &system_prompt_overrides,
                channel_permission_callbacks_for_mcp.as_ref(),
                &permission_sink_for_mcp,
                &has_interruptible_tool_in_progress,
                thinking_config_for_mcp.as_ref(),
                // The one path whose context never met the builder:
                // `submit_processed_prompt_deferred_query` builds it from
                // `with_permission_context` alone
                // (`handle_prompt_submit.rs:110-127`), so the builder-owned
                // fields are seeded from here instead.
                Some(loaded_nested_memory_paths.read().clone()),
            );
            if let Some(state) = next_content_replacement_state {
                content_replacement_state.set(state);
            }

            stream_mode.set(SpinnerMode::Responding);
            response_length_ref.set(0);
            active_query.set(Some(spawn_query(params, production_deps())));
            user_input_on_processing.set(None);
            let _ = mock_pump_tx_for_mcp.try_send(());
            }
        }
    });

    // Maps to: CC `commands/copy/copy.tsx::copyOrWriteToFile`. Keep OSC/file
    // transport async and component-bound rather than blocking a retained
    // update frame for large responses.
    let copy_direct_action = hooks.use_async_handler({
        let stdout_for_copy = command_stdout.clone();
        let mock_pump_tx_for_copy = mock_pump_tx.clone();
        let mut history_state_for_copy = history_state;
        let mut user_input_on_processing_for_copy = user_input_on_processing;
        move |(content, invocation): (
            crate::commands::copy::CopyContent,
            SlashCommandInvocation,
        )| {
            let stdout_for_copy = stdout_for_copy.clone();
            let mock_pump_tx_for_copy = mock_pump_tx_for_copy.clone();
            async move {
                let output = crate::commands::copy::copy::copy_or_write_to_file(
                    &stdout_for_copy,
                    &content.text,
                    &content.filename,
                )
                .await;
                let output = match output {
                    Ok(output) => output,
                    Err(error) => {
                        crate::utils::debug::log_for_debugging(&error.to_string());
                        return;
                    }
                };
                let invocation = LocalCommandInvocation {
                    slash_command: invocation,
                    dismiss_result: None,
                };
                append_local_command_invocation_result(
                    &mut history_state_for_copy,
                    &invocation,
                    &output,
                    false,
                );
                user_input_on_processing_for_copy.set(None);
                let _ = mock_pump_tx_for_copy.try_send(());
            }
        }
    });

    // CC commands/export/export.tsx call: await rendering before writing or
    // mounting JSX. Existing async handler preserves the REPL's event lifecycle.
    let export_action = hooks.use_async_handler({
        let mock_pump = mock_pump_tx.clone();
        let mut history = history_state;
        let mut processing = user_input_on_processing;
        let mut local_ui = active_local_command_ui;
        move |(args, context, invocation, preceding_input_blocks): (String, Arc<crate::tool::ToolUseContext>, SlashCommandInvocation, Vec<crate::types::message::UserContent>)| {
            let mock_pump = mock_pump.clone();
            async move {
                // Construct and render non-Send iocraft elements entirely on
                // one blocking worker; only the owned result crosses threads.
                let result = match crate::utils::process_runtime::runtime_handle_for_detached_work() {
                    Some(runtime) => runtime.spawn_blocking(move || futures::executor::block_on(
                        crate::commands::export::export::call(&context, &args)))
                        .await.map_err(std::io::Error::other).and_then(|result| result),
                    None => Err(std::io::Error::other("Process runtime unavailable for export")),
                };
                match result {
                    Ok(crate::commands::export::export::ExportResult::Done(output)) => {
                        let result = crate::utils::process_user_input::process_slash_command::local_jsx_command_result(
                            &invocation.command_name, &invocation.args, Some(&output), None,
                            false, &[], preceding_input_blocks);
                        append_history_rows(&mut history, result.messages);
                    },
                    Ok(crate::commands::export::export::ExportResult::Dialog(data)) => {
                        let current = local_ui.read().clone();
                        local_ui.set(set_local_command_ui(current,
                            ActiveLocalCommandUi::from_slash_command_with_source(
                                LocalCommandUi::Export { data, preceding_input_blocks }, invocation, false, false)));
                    },
                    Err(error) => crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string())),
                }
                processing.set(None);
                let _ = mock_pump.try_send(());
            }
        }
    });

    // Maps to: CC commands/reload-plugins/reload-plugins.ts async call and
    // processSlashCommand.tsx:859-947 local text/catch. A3/A6: the process
    // runtime owns refresh work; only the completion consumer is UI-bound.
    let reload_plugins_action = hooks.use_async_handler({
        let store = app_store.clone();
        let mock_pump = mock_pump_tx.clone();
        let mut history = history_state;
        let mut processing = user_input_on_processing;
        move |user_message: crate::types::message::UserMessage| {
            let store = store.clone();
            let mock_pump = mock_pump.clone();
            async move {
                let output = match crate::utils::process_runtime::runtime_handle_for_detached_work() {
                    Some(runtime) => runtime.spawn(async move {
                        crate::commands::reload_plugins::reload_plugins::call(&store).await
                    }).await.map_err(anyhow::Error::from).and_then(|result| result),
                    None => Err(anyhow::anyhow!("Process runtime unavailable for plugin reload")),
                };
                let output = output.map_err(|error| {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                    // The source catch uses String(e), retaining the Error name.
                    format!("Error: {error}")
                });
                let result = crate::utils::process_user_input::process_slash_command::local_text_command_result(
                    user_message, output);
                append_history_rows(&mut history, result.messages);
                processing.set(None);
                let _ = mock_pump.try_send(());
            }
        }
    });

    // Maps to: CC `REPL.tsx`'s retained async local-command completion path
    // (`processSlashCommand.tsx:859-947`). The advisor business callback is
    // owned by `src/commands/advisor/advisor.rs`; this block only keeps its
    // input envelope/context alive while validation finishes.
    let advisor_action = hooks.use_async_handler({
        let mock_pump = mock_pump_tx.clone();
        let mut history = history_state;
        let mut processing = user_input_on_processing;
        move |(
            args,
            context,
            invocation,
            user_message,
        ): (
            String,
            Arc<crate::tool::ToolUseContext>,
            SlashCommandInvocation,
            crate::types::message::UserMessage,
        )| {
            let mock_pump = mock_pump.clone();
            async move {
                let output = match crate::utils::process_runtime::runtime_handle_for_detached_work()
                {
                    Some(runtime) => runtime
                        .spawn(async move {
                            crate::commands::advisor::advisor::call(&args, &context).await
                        })
                        .await
                        .map_err(|error| format!("Error: {error}"))
                        .and_then(|result| result),
                    None => Err(
                        "Error: Process runtime unavailable for advisor validation".to_string(),
                    ),
                };
                let result = crate::utils::process_user_input::process_slash_command::local_text_command_result(
                    user_message,
                    output,
                );
                append_history_rows(&mut history, result.messages);
                let _ = invocation;
                processing.set(None);
                let _ = mock_pump.try_send(());
            }
        }
    });

    // Maps to: CC commands/terminalSetup/terminalSetup.tsx:174-222 and
    // processSlashCommand.tsx:675-855. A3/A6: the installer promise survives
    // component unmount, while its onDone consumer is component-bound.
    let terminal_setup_action = hooks.use_async_handler({
        let mock_pump = mock_pump_tx.clone();
        let mut history = history_state;
        let mut processing = user_input_on_processing;
        move |invocation: SlashCommandInvocation| {
            let mock_pump = mock_pump.clone();
            async move {
                let configured_theme = crate::utils::config::load_global_config().theme;
                let theme = configured_theme
                    .as_deref()
                    .and_then(crate::utils::theme::ThemeName::from_config_or_display)
                    .unwrap_or(crate::utils::theme::ThemeName::Dark);
                let result = match crate::utils::process_runtime::runtime_handle_for_detached_work()
                {
                    Some(runtime) => runtime
                        .spawn(async move {
                            crate::commands::terminal_setup::terminal_setup::call(theme).await
                        })
                        .await
                        .map_err(anyhow::Error::from)
                        .and_then(|result| result),
                    None => Err(anyhow::anyhow!(
                        "Process runtime unavailable for terminal setup"
                    )),
                };
                match result {
                    Ok(output) => append_local_command_invocation_result(
                        &mut history,
                        &LocalCommandInvocation {
                            slash_command: invocation,
                            dismiss_result: None,
                        },
                        &output,
                        false,
                    ),
                    // CC local-jsx rejection logs and resolves without rows;
                    // it must never become a successful command receipt.
                    Err(error) => crate::utils::log::log_error(crate::utils::log::LogError::new(
                        error.to_string(),
                    )),
                }
                processing.set(None);
                let _ = mock_pump.try_send(());
            }
        }
    });

    let color_action = hooks.use_async_handler({
        let mock_pump = mock_pump_tx.clone();
        let mut history = history_state;
        let mut processing = user_input_on_processing;
        let mut local_ui = active_local_command_ui;
        let notification_store = app_store.clone();
        move |(args, context, invocation, is_immediate): (String, Arc<crate::tool::ToolUseContext>, SlashCommandInvocation, bool)| {
            let mock_pump = mock_pump.clone();
            let notification_store = notification_store.clone();
            async move {
                match crate::commands::color::color::call(&context, &args).await {
                    Ok(output) => {
                        if is_immediate {
                            // CC REPL.tsx:4346-4381: immediate onDone clears
                            // local JSX, notifies, and escapes the output only
                            // in the non-fullscreen system transcript rows.
                            let current = local_ui.read().clone();
                            local_ui.set(clear_local_command_ui(current));
                            let mut notifications = crate::context::notifications::NotificationsWriter::new(notification_store);
                            notifications.add_notification(crate::context::notifications::Notification::text(
                                format!("immediate-{}", invocation.command_name),
                                output.clone(),
                                crate::context::notifications::NotificationPriority::Immediate,
                            ));
                            if !crate::utils::fullscreen::is_fullscreen_env_enabled() {
                                append_history_rows(&mut history, local_command_result_messages(
                                    &invocation.command_name,
                                    &invocation.args,
                                    &crate::utils::xml::escape_xml(&output),
                                    false,
                                ));
                            }
                        } else {
                            let result = crate::utils::process_user_input::process_slash_command::system_display_local_command_result(None, &invocation.command_name, &invocation.args, &output);
                            append_history_rows(&mut history, result.messages);
                        }
                    }
                    Err(error) => {
                        // CC processSlashCommand.tsx:843-855 resolves with
                        // no transcript rows on a local-jsx failure. The
                        // REPL immediate path has no onDone on rejection.
                        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                        if !is_immediate {
                            let current = local_ui.read().clone();
                            local_ui.set(clear_local_command_ui(current));
                        }
                    }
                }
                processing.set(None);
                let _ = mock_pump.try_send(());
            }
        }
    });

    // Maps to: CC `commands/rename/rename.ts` awaiting
    // `generateSessionName(...)`. The future is component-bound so model I/O
    // never blocks the retained update frame.
    let rename_generation_action = hooks.use_async_handler({
        let app_store_for_rename = app_store.clone();
        let mock_pump_tx_for_rename = mock_pump_tx.clone();
        let mut history_state_for_rename = history_state;
        let mut user_input_on_processing_for_rename = user_input_on_processing;
        move |(
            request,
            invocation,
        ): (
            crate::commands::rename::RenameGenerationRequest,
            SlashCommandInvocation,
        )| {
            let app_store_for_rename = app_store_for_rename.clone();
            let mock_pump_tx_for_rename = mock_pump_tx_for_rename.clone();
            async move {
                let abort_controller = crate::tool::AbortController::default();
                let generated = crate::commands::rename::generate_session_name::generate_session_name(
                    &request.messages,
                    &abort_controller,
                )
                .await;
                let output = match generated {
                    Some(new_name) => match crate::commands::rename::rename::save_session_name(
                        &request.session_id, &new_name, request.full_path.as_deref(),
                        &app_store_for_rename,
                    ) {
                        Ok(()) => Some(format!("Session renamed to: {new_name}")),
                        Err(error) => {
                            crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                            None
                        }
                    },
                    None => Some("Could not generate a name: no conversation context yet. Usage: /rename <name>".to_string()),
                };
                if let Some(output) = output {
                    let invocation = LocalCommandInvocation {
                        slash_command: invocation,
                        dismiss_result: None,
                    };
                    append_local_command_invocation_result(
                        &mut history_state_for_rename, &invocation, &output, false,
                    );
                }
                user_input_on_processing_for_rename.set(None);
                let _ = mock_pump_tx_for_rename.try_send(());
            }
        }
    });

    // The async half of `on_submit`. See `PromptQuerySubmit`: the callback
    // itself cannot await, and `handlePromptSubmit` now must.
    let prompt_query_action = hooks.use_async_handler({
        #[cfg(test)]
        let query_probe = active_query_probe.clone();
        let app_store_for_submit = app_store.clone();
        let prompt_submit_for_submit = prompt_submit.clone();
        let channel_permission_callbacks_for_submit = channel_permission_callbacks.clone();
        let permission_sink_for_submit = interactive_permission_sink.clone();
        let thinking_config_for_submit = thinking_config.clone();
        let system_prompt_overrides_for_submit = system_prompt_overrides.clone();
        let permission_context_for_submit_handler = permission_store.clone();
        let runtime_mcp_context_for_submit = runtime_mcp_context.clone();
        let initial_main_thread_agent_definition_for_submit =
            initial_main_thread_agent_definition.clone();
        let resume_processing_tx_for_submit = resume_processing_tx.clone();
        let context_processing_tx_for_submit = context_processing_tx.clone();
        let compact_processing_tx_for_submit = compact_processing_tx.clone();
        let plan_processing_tx_for_submit = plan_processing_tx.clone();
        let keybindings_processing_tx_for_submit = keybindings_processing_tx.clone();
        let initial_tools_for_submit = initial_tools.clone();
        let commands_for_submit = commands.clone();
        let copy_direct_action_for_submit = copy_direct_action.clone();
        let rename_generation_action_for_submit = rename_generation_action.clone();
        let color_action_for_submit = color_action.clone();
        let terminal_setup_action_for_submit = terminal_setup_action.clone();
        let export_action_for_submit = export_action.clone();
        let reload_plugins_action_for_submit = reload_plugins_action.clone();
        let advisor_action_for_submit = advisor_action.clone();
        let mock_pump_tx_for_submit = mock_pump_tx.clone();
        let context_terminal_cols_for_submit = terminal_cols;
        let mut history_state = history_state;
        let mut pending_responses = pending_responses;
        let mut active_query = active_query;
        let mut permission_queue = permission_queue;
        let mut content_replacement_state = content_replacement_state;
        let mut resume_restore_stores = resume_restore_stores;
        let mut loaded_nested_memory_paths_for_submit = loaded_nested_memory_paths;
        let main_thread_agent_definition_for_submit = main_thread_agent_definition;
        let terminal_title_disabled_for_submit = terminal_title_disabled;
        let haiku_title_attempted_for_submit_outer = Arc::clone(&haiku_title_attempted);
        let haiku_title_tx_for_submit_outer = haiku_title_tx.clone();
        let mut active_compact_abort_for_submit = active_compact_abort;
        let mut stream_mode = stream_mode;
        let mut response_length_ref = response_length_ref;
        let mut response_length_ref_for_submit = response_length_ref;
        let mut user_input_on_processing_for_submit = user_input_on_processing;
        let mut active_local_command_ui = active_local_command_ui;
        let mut message_selector_visible = message_selector_visible;
        let mut exit_flow_active = exit_flow_active;
        let ide_selection_for_submit = ide_selection;
        let has_interruptible_tool_in_progress = Arc::clone(&has_interruptible_tool_in_progress);
        move |request: PromptQuerySubmit| {
            #[cfg(test)]
            let query_probe = query_probe.clone();
            let app_store_for_submit = app_store_for_submit.clone();
            let prompt_submit_for_submit = prompt_submit_for_submit.clone();
            let haiku_title_attempted_for_submit =
                Arc::clone(&haiku_title_attempted_for_submit_outer);
            let haiku_title_tx_for_submit = haiku_title_tx_for_submit_outer.clone();
            let channel_permission_callbacks_for_submit =
                channel_permission_callbacks_for_submit.clone();
            let permission_sink_for_submit = permission_sink_for_submit.clone();
            let thinking_config_for_submit = thinking_config_for_submit.clone();
            let system_prompt_overrides = system_prompt_overrides_for_submit.clone();
            let permission_context_for_submit_handler =
                permission_context_for_submit_handler.clone();
            let runtime_mcp_context_for_submit = runtime_mcp_context_for_submit.clone();
            let initial_main_thread_agent_definition_for_submit =
                initial_main_thread_agent_definition_for_submit.clone();
            let resume_processing_tx_for_submit = resume_processing_tx_for_submit.clone();
            let context_processing_tx_for_submit = context_processing_tx_for_submit.clone();
            let compact_processing_tx_for_submit = compact_processing_tx_for_submit.clone();
            let plan_processing_tx_for_submit = plan_processing_tx_for_submit.clone();
            let keybindings_processing_tx_for_submit = keybindings_processing_tx_for_submit.clone();
            let initial_tools_for_submit = initial_tools_for_submit.clone();
            let commands_for_submit = commands_for_submit.clone();
            let copy_direct_action_for_submit = copy_direct_action_for_submit.clone();
            let rename_generation_action_for_submit = rename_generation_action_for_submit.clone();
            let color_action_for_submit = color_action_for_submit.clone();
            let terminal_setup_action_for_submit = terminal_setup_action_for_submit.clone();
            let export_action_for_submit = export_action_for_submit.clone();
            let reload_plugins_action_for_submit = reload_plugins_action_for_submit.clone();
            let advisor_action_for_submit = advisor_action_for_submit.clone();
            let mock_pump_tx_for_submit = mock_pump_tx_for_submit.clone();
            let has_interruptible_tool_in_progress =
                Arc::clone(&has_interruptible_tool_in_progress);
            async move {
                let PromptQuerySubmit {
                    text,
                    image_contents: submitted_image_contents,
                    image_paste_ids: submitted_image_ids,
                    from_keybinding,
                    is_query_active_for_immediate,
                    tool_permission_context: tool_permission_context_for_submit,
                    runtime_mcp_state: runtime_mcp_state_for_submit,
                    completed_local_command,
                } = request;
                // Rebuilt rather than passed: the callback's version closes over
                // the same `ide_selection` State, which is Copy.
                let mut clear_ide_selection_for_submit = {
                    let mut ide_selection_for_submit = ide_selection_for_submit;
                    move || {
                        if ide_selection_for_submit.read().is_some() {
                            ide_selection_for_submit.set(None);
                        }
                    }
                };
                // Maps to CC `handlePromptSubmit.ts::getToolUseContext(messages, [],
                // abortController, mainLoopModel)`. Slash callbacks such as `/copy`
                // and `/rename` must observe the live typed history/AppState before
                // processUserInput dispatches them.
                let main_thread_agent_for_command = main_thread_agent_definition_for_submit.read().clone();
                let command_context = build_repl_process_user_input_context(
                    tool_permission_context_for_submit,
                    &app_store_for_submit,
                    history_model_messages(&history_state.read()),
                    resume_restore_stores.read().clone(),
                    loaded_nested_memory_paths_for_submit.read().clone(),
                    runtime_mcp_state_for_submit.clone(),
                    initial_tools_for_submit.as_slice(),
                    commands_for_submit.clone(),
                    debug,
                    &system_prompt_overrides,
                    main_thread_agent_for_command.as_deref(),
                    channel_permission_callbacks_for_submit.as_ref(),
                    &permission_sink_for_submit,
                    thinking_config_for_submit.as_ref(),
                    repl_response_length_sink(response_length_ref_for_submit),
                );
                // CC `processTextPrompt.ts:66-88` mints ONE user message carrying the
                // text block, the image blocks and `imagePasteIds`; the per-image rows
                // come from `normalizeMessages` splitting that message per block (and
                // distributing the paste ids by image position), not from separately
                // pushed placeholder rows.
                let opening_continuation = LocalCommandContinuation {
                    preceding_input_blocks: submitted_image_contents.clone(),
                    input: text.clone(),
                    hook_session_id: command_context.agent_id.clone().unwrap_or_else(crate::bootstrap::state::get_session_id),
                    context: command_context.clone(),
                };
                let mut result = if let Some(completed) = completed_local_command {
                    prompt_submit_for_submit.complete_local_command_deferred_query(
                        completed.continuation.input,
                        completed.processed,
                        completed.continuation.context,
                        &completed.continuation.hook_session_id,
                    ).await
                } else { prompt_submit_for_submit
                    .submit_prompt_deferred_query_with_context(
                    text,
                    command_context,
                    submitted_image_contents,
                    submitted_image_ids,
                )
                .await };
                if matches!(&result.local_action, Some(SlashCommandAction::OpenLocalCommandUi { command: LocalCommandUi::Permissions, .. })) {
                    permissions_continuation.set(Some(opening_continuation));
                }
                let command_permission_context =
                    apply_repl_command_allowed_tools(&app_store_for_submit, &result.allowed_tools);
                // Maps to: CC `REPL.tsx:3585-3621` — extract a tab title from
                // the first real user message, one-shot via the attempted
                // gate. Synthetic breadcrumbs (slash-command output,
                // /commit-style expansions, local-command headers, bash-mode)
                // are not the user's topic; wait for real prose. On failure
                // the gate reopens so the next submission retries.
                if !terminal_title_disabled_for_submit
                    && crate::utils::session_storage::get_current_session_metadata()
                        .custom_title
                        .is_none()
                    && main_thread_agent_for_command.is_none()
                    && !haiku_title_attempted_for_submit.load(std::sync::atomic::Ordering::SeqCst)
                {
                    let first_user_text = result.messages.iter().find_map(|message| {
                        let RenderableMessageKind::User { message: user, .. } = &message.kind
                        else {
                            return None;
                        };
                        let text = user
                            .content
                            .iter()
                            .filter_map(|content| match content {
                                crate::types::message::UserContent::Text(text) => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            None
                        } else {
                            Some(text)
                        }
                    });
                    if let Some(text) = first_user_text {
                        let synthetic = [
                            crate::constants::xml::LOCAL_COMMAND_STDOUT_TAG,
                            crate::constants::xml::COMMAND_MESSAGE_TAG,
                            crate::constants::xml::COMMAND_NAME_TAG,
                            crate::constants::xml::BASH_INPUT_TAG,
                        ]
                        .iter()
                        .any(|tag| text.starts_with(&format!("<{tag}>")));
                        if !synthetic {
                            haiku_title_attempted_for_submit
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                            let _ = haiku_title_tx_for_submit.try_send(text);
                        }
                    }
                }
                let local_action = result.local_action.clone();
                // CC REPL.tsx:4302-4310: the live-query gate combines the
                // descriptor's immediate flag with an explicit keybinding.
                let color_is_immediate = is_query_active_for_immediate
                    && match &local_action {
                        Some(SlashCommandAction::SetSessionColor { invocation, .. }) =>
                            from_keybinding || crate::commands::find_command(&invocation.command_name, &commands_for_submit)
                                .is_some_and(|command| command.immediate),
                        _ => false,
                    };
                // Maps to: CC REPL.tsx:4429 vs :4525 — opening a local command UI while
                // a query is active is the immediate local-jsx command path, which
                // returns early WITHOUT `setIDESelection(undefined)`; every other
                // resolution processes the submission now and clears.
                if !(color_is_immediate || (is_query_active_for_immediate
                    && matches!(
                        local_action,
                        Some(SlashCommandAction::OpenLocalCommandUi { .. })
                    )))
                {
                    clear_ide_selection_for_submit();
                }
                let query_params = result.query_params.clone();

                if matches!(local_action, Some(SlashCommandAction::ClearConversation)) {
                    // Official `/clear` first clears conversation state, then emits the
                    // local command transcript rows. Session file pointer / hook message
                    // injection still thin — see `commands/clear/conversation.rs`.
                    pending_responses.set(Vec::new());
                    if let Some(handle) = active_query.read().clone() {
                        let _ = handle.commands.try_send(QueryCommand::Abort);
                        handle.abort_controller.abort();
                    }
                    active_query.set(None);
                    permission_queue.set(Vec::new());
                    // CC clear/conversation.ts:170-200 spreads prev AppState:
                    // retain directory/rule grants rather than rerunning startup.
                    resume_restore_stores.set(ResumeRestoreStores::default());
                    loaded_nested_memory_paths_for_submit.set(std::collections::HashSet::new());
                    // Maps to CC `clearConversation` (AppState + caches + session regen + hooks).
                    let clear_outcome = crate::commands::clear::clear_conversation(&app_store_for_submit);
                    let mut cleared_messages = result.messages;
                    // CC: if (hookMessages.length > 0) setMessages(() => hookMessages)
                    // — after clear, SessionStart hooks may inject context rows.
                    cleared_messages.extend(clear_outcome.session_start_messages);
                    // The command/hook rows are System kinds → render-only `Row`
                    // entries, so the post-clear model projection is empty (the old
                    // explicit `model_history = []`).
                    history_state.set(Arc::new(
                        cleared_messages
                            .into_iter()
                            .map(history_entry_from_row)
                            .collect::<Vec<_>>(),
                    ));
                    content_replacement_state
                        .set(crate::utils::tool_result_storage::ContentReplacementState::new());
                } else {
                    append_prompt_submit_result(&mut history_state, &mut pending_responses, result);
                    if let Some(mut params) = query_params {
                        // The synchronous submit owner has already queued the
                        // ordinary active-turn prompt/bash case. Preserve the
                        // original conditional interrupt guard for races and
                        // immediate commands; a blocking tool must keep running.
                        if has_interruptible_tool_in_progress
                            .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            if let Some(handle) = active_query.read().clone() {
                                // Publish the DOM-style abort reason before waking
                                // the query actor; ShellCommand must observe
                                // `interrupt` rather than a racing generic abort.
                                handle.abort_controller.abort_with_reason("interrupt");
                                let _ = handle.commands.try_send(QueryCommand::Abort);
                            }
                        }
                        // C3c-4: the submitted user rows already entered the ONE
                        // history as `Message` entries; both query views are
                        // projections of it. Pasted images are blocks of that submitted
                        // message (CC `processTextPrompt.ts:66-88`), so there is no
                        // separate model-only carrier to splice in here.
                        let entries_snapshot = Arc::clone(&*history_state.read());
                        let main_thread_agent_definition =
                            main_thread_agent_definition_for_submit.read().clone();
                        // Maps to: CC REPL.tsx:3703-3714, both checks settle before the query starts.
                        let gate_snapshot = app_store_for_submit.get();
                        tokio::join!(
                            crate::utils::permissions::bypass_permissions_killswitch::check_and_disable_bypass_permissions_if_needed(
                                &command_permission_context, &app_store_for_submit),
                            crate::utils::permissions::bypass_permissions_killswitch::check_and_disable_auto_mode_if_needed(
                                &command_permission_context, &app_store_for_submit, Some(gate_snapshot.fast_mode)),
                        );
                        let next_content_replacement_state = apply_repl_query_turn_context(
                            &mut params,
                            &app_store_for_submit,
                            command_permission_context,
                            &entries_snapshot,
                            content_replacement_state.read().clone(),
                            &resume_restore_stores.read(),
                            runtime_mcp_context_for_submit
                                .as_ref()
                                .map(|context| context.current()),
                            main_thread_agent_definition.as_deref(),
                            initial_tools_for_submit.as_slice(),
                            &system_prompt_overrides,
                            channel_permission_callbacks_for_submit.as_ref(),
                            &permission_sink_for_submit,
                            &has_interruptible_tool_in_progress,
                            thinking_config_for_submit.as_ref(),
                            // Context came from `build_repl_process_user_input_context`
                            // above, which already applied the builder-owned fields.
                            None,
                        );
                        if let Some(state) = next_content_replacement_state {
                            content_replacement_state.set(state);
                        }

                        stream_mode.set(SpinnerMode::Responding);
                        response_length_ref.set(0);
                        #[cfg(test)]
                        let query_handle = if let Some(probe) = query_probe.as_ref() {
                            probe.started_queries.lock().unwrap().push(params);
                            probe.handle.clone()
                        } else { spawn_query(params, production_deps()) };
                        #[cfg(not(test))]
                        let query_handle = spawn_query(params, production_deps());
                        active_query.set(Some(query_handle));
                    }
                    // No-query path: the submitted user rows already entered the one
                    // history as `Message` entries above, so the old explicit
                    // model-history extend is covered by the projection.
                }

                let keeps_user_input_processing = matches!(
                    &local_action,
                    Some(
                        SlashCommandAction::SetSessionColor { .. }
                            | SlashCommandAction::Advisor { .. }
                            | SlashCommandAction::ReloadPlugins { .. }
                            | SlashCommandAction::SetupTerminal { .. }
                            | SlashCommandAction::ExportConversation { .. }
                            | SlashCommandAction::BranchConversation { .. }
                            | SlashCommandAction::CopyToClipboard { .. }
                            | SlashCommandAction::GenerateSessionName { .. }
                            | SlashCommandAction::AnalyzeContext { .. }
                            | SlashCommandAction::CompactConversation { .. }
                            | SlashCommandAction::InspectPlan { .. }
                            | SlashCommandAction::EditKeybindings { .. }
                    )
                );
                match local_action {
                    Some(SlashCommandAction::Advisor {
                        args,
                        context,
                        invocation,
                        user_message,
                    }) => {
                        advisor_action_for_submit((args, context, invocation, user_message));
                    }
                    Some(SlashCommandAction::ReloadPlugins { user_message, .. }) => {
                        reload_plugins_action_for_submit(user_message);
                    }
                    Some(SlashCommandAction::ExportConversation { args, context, invocation, preceding_input_blocks }) => {
                        export_action_for_submit((args, context, invocation, preceding_input_blocks));
                    }
                    Some(SlashCommandAction::SetupTerminal { invocation }) => {
                        terminal_setup_action_for_submit(invocation);
                    }
                    Some(SlashCommandAction::OpenLocalCommandUi {
                        command,
                        invocation,
                    }) => {
                        let current = active_local_command_ui.read().clone();
                        active_local_command_ui.set(set_local_command_ui(
                            current,
                            ActiveLocalCommandUi::from_slash_command_with_source(
                                command,
                                invocation,
                                is_query_active_for_immediate,
                                from_keybinding,
                            ),
                        ));
                    }
                    Some(SlashCommandAction::BranchConversation { args, invocation }) => {
                        let invocation = LocalCommandInvocation {
                            slash_command: invocation,
                            dismiss_result: None,
                        };
                        let app_snapshot = app_store_for_submit.get();
                        let agent_type = main_thread_agent_definition_for_submit
                            .read().as_ref().map(|agent| agent.agent_type.clone());
                        let model = Some(repl_main_loop_model(&app_snapshot));
                        let initial_agent = initial_main_thread_agent_definition_for_submit.clone();
                        let agent_definitions = app_snapshot.agent_definitions.clone();
                        let sender = resume_processing_tx_for_submit.clone();
                        std::thread::spawn(move || {
                            let prepared = crate::commands::branch::branch::call(&args);
                            let (success_message, result) = match prepared {
                                Ok(prepared) => (
                                    Some(prepared.success_message),
                                    resume(prepared.target, initial_agent, agent_definitions, agent_type, model),
                                ),
                                Err(error) => (None, Err(error)),
                            };
                            let result = result.map_err(|error| format!("Failed to branch conversation: {error}"));
                            let _ = sender.send_blocking((Some(invocation), success_message, result, None));
                        });
                    }
                    Some(SlashCommandAction::ResumeByArg { arg, invocation }) => {
                        let invocation = LocalCommandInvocation {
                            slash_command: invocation,
                            dismiss_result: None,
                        };
                        let app_snapshot = app_store_for_submit.get();
                        let agent_type = main_thread_agent_definition_for_submit
                            .read()
                            .as_ref()
                            .map(|agent| agent.agent_type.clone());
                        let model = Some(repl_main_loop_model(&app_snapshot));
                        let initial_agent = initial_main_thread_agent_definition_for_submit.clone();
                        let agent_definitions = app_snapshot.agent_definitions.clone();
                        let sender = resume_processing_tx_for_submit.clone();
                        let project_path = crate::bootstrap::state::get_original_cwd()
                            .display()
                            .to_string();
                        std::thread::spawn(move || {
                            let result = resume::load_for_arg(&project_path, &arg)
                                .map_err(|error| error.to_string())
                                .and_then(|target| {
                                    resume(target, initial_agent, agent_definitions, agent_type, model)
                                });
                            let _ = sender.send_blocking((Some(invocation), None, result, None));
                        });
                    }
                    Some(SlashCommandAction::SetSessionColor { args, context, invocation }) => { color_action_for_submit((args, context, invocation, color_is_immediate)); }
                    Some(SlashCommandAction::CopyToClipboard {
                        content,
                        invocation,
                    }) => {
                        copy_direct_action_for_submit((content, invocation));
                    }
                    Some(SlashCommandAction::GenerateSessionName {
                        request,
                        invocation,
                    }) => {
                        rename_generation_action_for_submit((request, invocation));
                    }
                    Some(SlashCommandAction::AnalyzeContext {
                        mut request,
                        invocation,
                    }) => {
                        request.terminal_width = Some(context_terminal_cols_for_submit.max(1));
                        request.system_prompt_overrides = system_prompt_overrides.clone();
                        request.main_thread_agent_definition = main_thread_agent_definition_for_submit
                            .read()
                            .clone()
                            .map(Arc::unwrap_or_clone);
                        let sender = context_processing_tx_for_submit.clone();
                        std::thread::spawn(move || {
                            let result = crate::commands::context::context::call(&request);
                            let _ = sender.send_blocking((invocation, result));
                        });
                    }
                    Some(SlashCommandAction::CompactConversation {
                        custom_instructions,
                        context,
                        invocation,
                    }) => {
                        let context = Arc::unwrap_or_clone(context);
                        let messages = context.messages.clone();
                        let main_thread_agent = main_thread_agent_definition_for_submit.read().clone();
                        let prompt_overrides = system_prompt_overrides.clone();
                        let abort_controller = context.abort_controller.clone();
                        active_compact_abort_for_submit.set(Some(abort_controller));
                        stream_mode.set(SpinnerMode::Requesting);
                        response_length_ref_for_submit.set(0);
                        let sender = compact_processing_tx_for_submit.clone();
                        let worker_sender = sender.clone();
                        let worker_invocation = invocation.clone();
                        let spawn_result = std::thread::Builder::new()
                            .name("cometix-compact".to_string())
                            .stack_size(8 * 1024 * 1024)
                            .spawn(move || {
                            let result = match tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                            {
                                Ok(runtime) => runtime.block_on(async {
                                    crate::commands::compact::compact::call(
                                        messages,
                                        &context,
                                        || {
                                            let model = context.main_loop_model.clone().unwrap_or_else(
                                                crate::utils::model::model::get_main_loop_model,
                                            );
                                            // Maps to CC `REPL.tsx:6927-6935`: the compact-family
                                            // prompt rebuild reads `context.getAppState()
                                            // .toolPermissionContext.additionalWorkingDirectories`
                                            // and `context.options.mcpClients`. Without a live
                                            // store the context snapshot is the same map.
                                            let additional_working_directories: Vec<String> = context
                                                .get_app_state()
                                                .map(|state| {
                                                    state
                                                        .tool_permission_context
                                                        .additional_working_directories
                                                        .keys()
                                                        .cloned()
                                                        .collect()
                                                })
                                                .unwrap_or_else(|| {
                                                    context
                                                        .tool_permission_context
                                                        .additional_working_directories
                                                        .keys()
                                                        .cloned()
                                                        .collect()
                                                });
                                            let (system_prompt, user_context, system_context) =
                                                build_repl_query_prompt_contexts(
                                                    &prompt_overrides,
                                                    crate::constants::prompts::get_system_prompt(
                                                        &context.tools,
                                                        &model,
                                                        &additional_working_directories,
                                                        &context.mcp_state.clients,
                                                    ),
                                                    main_thread_agent.as_deref(),
                                                    &context.mcp_state,
                                                );
                                            crate::commands::compact::compact::CompactCommandCacheContext {
                                                system_prompt,
                                                user_context,
                                                system_context,
                                            }
                                        },
                                        &custom_instructions,
                                    )
                                    .await
                                    .map_err(|error| {
                                        if context.abort_controller.is_aborted() {
                                            "Compaction canceled.".to_string()
                                        } else {
                                            error
                                        }
                                    })
                                }),
                                Err(error) => Err(format!("Failed to start compact runtime: {error}")),
                            };
                            let _ = worker_sender.send_blocking((worker_invocation, result));
                        });
                        if let Err(error) = spawn_result {
                            let _ = sender.send_blocking((
                                invocation,
                                Err(format!("Failed to start compact worker: {error}")),
                            ));
                        }
                    }
                    Some(SlashCommandAction::ApplyPlanMode { .. }) => {
                        // The command callback already committed the AppStore update;
                        // HandlePromptSubmit copied this context into QueryParams.
                    }
                    Some(SlashCommandAction::InspectPlan {
                        request,
                        invocation,
                    }) => {
                        let sender = plan_processing_tx_for_submit.clone();
                        std::thread::spawn(move || {
                            let result = crate::commands::plan::plan::inspect_plan(&request);
                            let _ = sender.send_blocking((invocation, result));
                        });
                    }
                    Some(SlashCommandAction::EditKeybindings { invocation }) => {
                        let sender = keybindings_processing_tx_for_submit.clone();
                        std::thread::spawn(move || {
                            let result =
                                crate::commands::keybindings::keybindings::prepare_keybindings_file();
                            let _ = sender.send_blocking((invocation, result));
                        });
                    }
                    Some(SlashCommandAction::ClearConversation) => {
                        let current = active_local_command_ui.read().clone();
                        active_local_command_ui.set(clear_local_command_ui(current));
                    }
                    Some(SlashCommandAction::OpenMessageSelector) => {
                        // Maps to: CC REPL.tsx:3238 `openMessageSelector` (rewind.ts
                        // → context.openMessageSelector → setIsMessageSelectorVisible).
                        message_selector_visible.set(true);
                    }
                    Some(SlashCommandAction::Exit) => {
                        if let Some(handle) = active_query.read().clone() {
                            let _ = handle.commands.try_send(QueryCommand::Abort);
                            handle.abort_controller.abort();
                        }
                        active_query.set(None);
                        permission_queue.set(Vec::new());
                        let current = active_local_command_ui.read().clone();
                        active_local_command_ui.set(clear_local_command_ui(current));
                        exit_flow_active.set(true);
                    }
                    None => {}
                }

                if !keeps_user_input_processing {
                    user_input_on_processing_for_submit.set(None);
                }
                let _ = mock_pump_tx_for_submit.try_send(());
            }
        }
    });

    // Maps to: CC REPL.tsx onSubmit -> handlePromptSubmit.
    // Repl owns UI state and executes typed local command UI actions returned
    // by the handle_prompt_submit/process_user_input/process_slash_command seam.
    let mock_pump_tx_for_submit = mock_pump_tx.clone();
    let prompt_submit_for_submit = prompt_submit.clone();
    let channel_permission_callbacks_for_submit = channel_permission_callbacks.clone();
    let app_store_for_submit = app_store.clone();
    let thinking_config_for_submit = thinking_config.clone();
    let system_prompt_overrides_for_submit = system_prompt_overrides.clone();
    let mut prompt_input_generation_for_submit = prompt_input_generation;
    let mut restored_input_for_submit = restored_input;
    let permission_context_for_submit_handler = permission_store.clone();
    let runtime_mcp_context_for_submit = runtime_mcp_context.clone();
    let prompt_shell_sender_for_submit = prompt_shell_channel.0.clone();
    let mut active_prompt_shell_for_submit = active_prompt_shell_command;
    let initial_main_thread_agent_definition_for_submit =
        initial_main_thread_agent_definition.clone();
    let main_thread_agent_definition_for_submit = main_thread_agent_definition;
    let mut loaded_nested_memory_paths_for_submit = loaded_nested_memory_paths;
    let resume_processing_tx_for_submit = resume_processing_tx.clone();
    let context_processing_tx_for_submit = context_processing_tx.clone();
    let speculation_acceptance_tx_for_submit = speculation_acceptance_tx.clone();
    let compact_processing_tx_for_submit = compact_processing_tx.clone();
    let mut active_compact_abort_for_submit = active_compact_abort;
    let plan_processing_tx_for_submit = plan_processing_tx.clone();
    let keybindings_processing_tx_for_submit = keybindings_processing_tx.clone();
    let context_terminal_cols_for_submit = terminal_cols;
    let initial_tools_for_submit = initial_tools.clone();
    let commands_for_submit = commands.clone();
    let copy_direct_action_for_submit = copy_direct_action.clone();
    let rename_generation_action_for_submit = rename_generation_action.clone();
    let mut user_input_on_processing_for_submit = user_input_on_processing;
    let mut response_length_ref_for_submit = response_length_ref;
    let ide_selection_for_submit = ide_selection;
    let has_interruptible_tool_in_progress_for_submit =
        Arc::clone(&has_interruptible_tool_in_progress);
    let pending_responses_for_submit = pending_responses;
    let active_query_for_submit = active_query;
    let permission_queue_for_submit = permission_queue;
    let prompt_query_action_for_submit = prompt_query_action.clone();
    let mut on_submit = move |submission: PromptSubmission| {
        let system_prompt_overrides = system_prompt_overrides_for_submit.clone();
        let from_keybinding = submission.from_keybinding;
        let submitted_image_ids = submission
            .pasted_contents
            .values()
            .filter_map(|content| match content {
                crate::components::prompt_input::input_paste::PastedContent::Image {
                    id,
                    data: Some(_),
                    ..
                } => Some(*id as u32),
                _ => None,
            })
            .collect::<Vec<_>>();
        let submitted_image_contents =
            crate::components::prompt_input::input_paste::image_content_blocks(
                &submission.pasted_contents,
            );
        let text = crate::components::prompt_input::input_paste::expand_text_references(
            &submission.text,
            &submission.pasted_contents,
        );
        let query_guard_active = !pending_responses_for_submit.read().is_empty()
            || active_query_for_submit.read().is_some()
            || !permission_queue_for_submit.read().is_empty();
        let is_bash_mode = text.trim_start().starts_with('!');
        let input_mode = if is_bash_mode { "bash" } else { "prompt" };
        let immediate_local_ui =
            crate::utils::handle_prompt_submit::is_immediate_local_ui_submission(
                &text,
                &commands_for_submit,
                query_guard_active,
                from_keybinding,
            );
        if crate::utils::handle_prompt_submit::should_queue_submitted_input(
            input_mode,
            query_guard_active,
            false,
            immediate_local_ui,
        ) {
            // Maps to CC `handlePromptSubmit.ts` queue branch: interrupt only
            // when every running tool opted into `cancel`; otherwise leave the
            // current query untouched and let the idle queue processor drain
            // this command later.
            if has_interruptible_tool_in_progress_for_submit
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                if let Some(handle) = active_query_for_submit.read().clone() {
                    handle.abort_controller.abort_with_reason("interrupt");
                    let _ = handle.commands.try_send(QueryCommand::Abort);
                }
            }
            if let Some(queued) = crate::utils::handle_prompt_submit::queued_command_for_submission(
                &text,
                &submission.text,
                submission.pasted_contents.clone(),
                input_mode,
            ) {
                crate::utils::message_queue_manager::enqueue(queued);
            }
            restored_input_for_submit.set(None);
            prompt_input_generation_for_submit
                .set(prompt_input_generation_for_submit.get().wrapping_add(1));
            user_input_on_processing_for_submit.set(None);
            return;
        }
        user_input_on_processing_for_submit.set(Some(text.clone()));
        restored_input_for_submit.set(None);
        prompt_input_generation_for_submit
            .set(prompt_input_generation_for_submit.get().wrapping_add(1));
        // Maps to: CC REPL.tsx:4525 `setIDESelection(undefined)` — the clear
        // lives inside the `submitsNow` block (REPL.tsx:4506-4508), so only
        // submissions that process now clear the ide selection to None
        // (distinct from the identity-change reset OBJECT,
        // useIdeSelection.ts:77-82). CC's immediate local-jsx command path
        // returns early WITHOUT clearing (REPL.tsx:4429); the Cometix analog
        // is `OpenLocalCommandUi` while a query is active, so the clear is
        // applied per-path below instead of unconditionally up front.
        let mut clear_ide_selection_for_submit = {
            let mut ide_selection_for_submit = ide_selection_for_submit;
            move || {
                if ide_selection_for_submit.read().is_some() {
                    ide_selection_for_submit.set(None);
                }
            }
        };
        if let Some(command) = text
            .strip_prefix('!')
            .map(str::trim)
            .filter(|command| !command.is_empty())
        {
            // Bash-mode submissions process now (CC `submitsNow` → REPL.tsx:4525).
            clear_ide_selection_for_submit();
            let command = command.to_string();
            active_prompt_shell_for_submit.set(Some(command.clone()));
            let sender = prompt_shell_sender_for_submit.clone();
            let shell_store = app_store_for_submit.clone();
            user_input_on_processing_for_submit.set(None);
            std::thread::spawn(move || {
                let args = serde_json::json!({
                    "command": command,
                    "dangerouslyDisableSandbox": true,
                });
                let context = crate::tool::ToolUseContext::default().with_app_store(shell_store);
                let result = crate::tools::bash_tool::bash_output(&args, &context, None, None);
                let command = args
                    .get("command")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let _ = sender.send_blocking((command, result));
            });
            return;
        }
        let accepted_speculation = {
            let snapshot = app_store_for_submit.get();
            match &snapshot.speculation {
                crate::state::app_state_store::SpeculationState::Active(active)
                    if snapshot.prompt_suggestion.text.as_deref() == Some(text.as_str()) =>
                {
                    Some(active.clone())
                }
                _ => None,
            }
        };
        if let Some(active) = accepted_speculation {
            // Speculation accept forces `submitsNow` (REPL.tsx:4506-4508 → :4525).
            clear_ide_selection_for_submit();
            classifier_approvals.set(ClassifierApprovalsState::default());
            streaming_text_preview.set(None);
            user_input_on_processing_for_submit.set(None);
            // B3 flip-audit: CC speculation.ts:852-857 — `if (text === null
            // && promptId === null) return prev` before the clearing spread.
            app_store_for_submit.set_state(|prev| {
                if prev.prompt_suggestion.text.is_none()
                    && prev.prompt_suggestion.prompt_id.is_none()
                {
                    return crate::state::store::UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.prompt_suggestion =
                    crate::state::app_state_store::PromptSuggestionState::default();
                crate::state::store::UpdateDecision::Replace {
                    next: Arc::new(next),
                    result: (),
                }
            });
            // C3c-4: one `Message` entry carries both the accepted user row
            // and its model history (the dual write with two different uuids
            // collapsed into CC's single-array push).
            let user_message = Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(text.clone())],
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
            let mut entries = history_state.read().as_ref().clone();
            entries.push(HistoryEntry::Message(user_message));
            history_state.set(Arc::new(entries));
            let clean_messages =
                crate::services::prompt_suggestion::speculation::prepare_messages_for_injection(
                    &active
                        .messages
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
            let context = active.cache_safe_params.tool_use_context.clone();
            let sender = speculation_acceptance_tx_for_submit.clone();
            tokio::spawn(async move {
                let result = Some(
                    crate::services::prompt_suggestion::speculation::accept_active_speculation(
                        &context,
                        &active,
                        clean_messages.len(),
                    )
                    .await,
                );
                let _ = sender
                    .send(SpeculationAcceptanceOutcome {
                        input: text,
                        clean_messages,
                        result,
                        active,
                    })
                    .await;
            });
            return;
        }
        let tool_permission_context_for_submit =
            permission_context_for_submit_handler.tool_permission_context();
        let is_query_active_for_immediate =
            !pending_responses.read().is_empty() || active_query.read().is_some();
        classifier_approvals.set(ClassifierApprovalsState::default());
        streaming_text_preview.set(None);
        // Maps to: CC clearing `tipPickedThisTurnRef` on user submit.
        crate::services::tips::reset_tip_picked_this_turn();

        let runtime_mcp_state_for_submit = runtime_mcp_context_for_submit
            .as_ref()
            .map(|context| context.current())
            .unwrap_or_default();
        if let Some(parsed) = crate::utils::slash_command_parsing::parse_slash_command(&text) {
            if let Some((server_name, prompt)) =
                crate::services::mcp::client::resolve_mcp_prompt_command_invocation(
                    &parsed.name,
                    &runtime_mcp_state_for_submit,
                )
            {
                // MCP prompt slash commands dispatch a query now (REPL.tsx:4525).
                clear_ide_selection_for_submit();
                let command = crate::services::mcp::client::mcp_prompt_command_snapshot(
                    &server_name,
                    &prompt,
                );
                mcp_prompt_submit_action(McpPromptSlashCommandSubmit {
                    input: text,
                    turn_id: Uuid::new_v4().to_string(),
                    command,
                    server_name,
                    prompt,
                    args: parsed.args,
                    tool_permission_context: tool_permission_context_for_submit,
                });
                return;
            }
        }

        prompt_query_action_for_submit(PromptQuerySubmit {
            text,
            image_contents: submitted_image_contents,
            image_paste_ids: submitted_image_ids,
            from_keybinding,
            is_query_active_for_immediate,
            tool_permission_context: tool_permission_context_for_submit,
            runtime_mcp_state: runtime_mcp_state_for_submit,
            completed_local_command: None,
        });
    };

    // Maps to: CC `CommandKeybindingHandlers` in both REPL render branches.
    // The hook records `/<name>`; invoking the normal submit owner here keeps
    // PromptInput's existing buffer mounted and therefore preserves it.
    // Hoisted out of the `&&` chain below: `&&` short-circuits, and this is a
    // hook — iocraft resolves hooks by call index, so a skipped call shifts
    // every later hook in the component and panics at render.
    let modal_overlay_active =
        crate::context::overlay_context::use_is_modal_overlay_active(&mut hooks);
    let command_keybindings_active = active_local_command_ui
        .read()
        .as_ref()
        .is_none_or(|active| !active.is_local_command_ui)
        && permission_queue.read().is_empty()
        && classifier_approvals.read().checking().is_none()
        && !message_selector_visible.get()
        && !exit_flow_active.get()
        && !show_remote_callout
        && !show_desktop_upsell_startup.get()
        && hint_recommendation.read().is_none()
        && !prompt_modal_overlay_active.get()
        && !modal_overlay_active;
    let mut command_keybinding_handlers =
        crate::hooks::use_command_keybindings::use_command_keybinding_handlers(
            &mut hooks,
            keybinding_runtime_for_command_handlers,
            command_keybindings_active,
        );
    if let Some(command) = command_keybinding_handlers.take_pending_command() {
        on_submit(PromptSubmission {
            text: command,
            pasted_contents: std::collections::BTreeMap::new(),
            from_keybinding: true,
        });
    }

    // Maps to: CC `REPL.tsx` `useInboxPoller({ enabled, isLoading,
    // focusedInputDialog, onSubmitMessage: handleIncomingPrompt })`.
    let inbox_poll_interval = crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
        .then_some(Duration::from_millis(
            crate::hooks::use_inbox_poller::INBOX_POLL_INTERVAL_MS,
        ));
    hooks.use_interval(
        {
            let runtime_mcp_context = runtime_mcp_context.clone();
            let prompt_submit_for_inbox = prompt_submit.clone();
            let mock_pump_tx_for_inbox = mock_pump_tx.clone();
            let mut inbox_poller_state = inbox_poller_state;
            let mut history_state = history_state;
            let mut pending_responses = pending_responses;
            let mut active_query = active_query;
            let mut permission_queue = permission_queue;
            let permission_store = permission_store.clone();
            let mut content_replacement_state = content_replacement_state;
            let resume_restore_stores = resume_restore_stores;
            let loaded_nested_memory_paths = loaded_nested_memory_paths;
            let main_thread_agent_definition = main_thread_agent_definition;
            let system_prompt_overrides = system_prompt_overrides.clone();
            let active_local_command_ui = active_local_command_ui;
            let show_desktop_upsell_startup = show_desktop_upsell_startup;
            let hint_recommendation = hint_recommendation;
            let exit_flow_active = exit_flow_active;
            let channel_permission_callbacks_for_inbox = channel_permission_callbacks.clone();
            let permission_sink_for_inbox = interactive_permission_sink.clone();
            let app_store_for_inbox = app_store.clone();
            let thinking_config_for_inbox = thinking_config.clone();
            let initial_tools_for_inbox = initial_tools.clone();
            let commands_for_inbox = commands.clone();
            let response_length_ref_for_inbox = response_length_ref;
            let has_interruptible_tool_in_progress =
                Arc::clone(&has_interruptible_tool_in_progress);
            move || {
                let enabled = crate::utils::agent_swarms_enabled::is_agent_swarms_enabled();
                if !enabled {
                    return;
                }

                let is_loading_now = !pending_responses.read().is_empty()
                    || active_query.read().is_some()
                    || !permission_queue.read().is_empty();
                let show_remote_callout = app_store_for_inbox.get().show_remote_callout;
                let focused_input_dialog = (!permission_queue.read().is_empty()
                    // CC useInboxPoller receives 'sandbox-permission' via
                    // focusedInputDialog (use_inbox_poller.tsx:572,597).
                    || !sandbox_permission_request_queue.read().is_empty()
                    || active_local_command_ui.read().is_some()
                    || show_remote_callout
                    || show_desktop_upsell_startup.get()
                    || hint_recommendation.read().is_some()
                    || exit_flow_active.get())
                .then_some("dialog".to_string());

                let poller_local = inbox_poller_state.read().clone();
                let app_snapshot = app_store_for_inbox.get();
                // P4 identity: the poll core is by-value (InboxPollMutable), so
                // unwrap the AppState Arcs into owned copies here; fresh Arcs
                // are minted only at the write-back boundary below.
                let mut state =
                    crate::hooks::use_inbox_poller::InboxPollMutable::from_app_and_poller(
                        (*app_snapshot.inbox).clone(),
                        (*app_snapshot.worker_sandbox_permissions).clone(),
                        app_snapshot.pending_sandbox_request.as_deref().cloned(),
                        poller_local,
                    );
                let mut team_context =
                    crate::hooks::use_inbox_poller::current_team_context_from_team_record();
                let mut permission_context = permission_store.tool_permission_context();

                let delivered = crate::hooks::use_inbox_poller::deliver_pending_messages_when_idle(
                    &mut state,
                    team_context.as_ref(),
                    enabled,
                    is_loading_now,
                    focused_input_dialog.as_deref(),
                    true,
                );
                let outcome = crate::hooks::use_inbox_poller::poll_inbox_once(
                    &mut state,
                    &mut team_context,
                    &mut permission_context,
                    &crate::hooks::use_inbox_poller::InboxPollerConfig {
                        enabled,
                        is_loading: is_loading_now || delivered.is_some(),
                        focused_input_dialog: focused_input_dialog.clone(),
                        idle_submit_accepted: true,
                    },
                );

                if permission_context != permission_store.tool_permission_context() {
                    permission_store.set_tool_permission_context(permission_context);
                }
                if !state.poller.permission_queue.is_empty() {
                    let mut queue = permission_queue.read().clone();
                    for confirm in state.poller.permission_queue.drain(..) {
                        if !queue
                            .iter()
                            .any(|queued| queued.tool_use_id() == confirm.tool_use_id())
                        {
                            queue.push(confirm);
                        }
                    }
                    permission_queue.set(queue);
                }
                for kill in std::mem::take(&mut state.poller.pending_pane_kills) {
                    crate::utils::swarm::backends::registry::spawn_kill_pane_for_shutdown(
                        kill.backend_type.as_deref(),
                        kill.pane_id,
                        kill.agent_id,
                    );
                }

                let submitted = delivered.or(outcome.submitted);
                // Single source: AppState bags (CC inbox / workerSandbox /
                // pendingSandbox / teamContext).
                // P3 §3a: the poller ticks continuously — an unchanged tick
                // must return `prev` untouched or every poll schedules a
                // render pass after the B3 flip (render storm).
                app_store_for_inbox.set_state(|prev| {
                    // P4 identity: this loop rebuilds the bags every tick, so
                    // the unchanged-tick guard MUST compare by value — after
                    // the Arc migration `Arc::ptr_eq` against a rebuilt value
                    // would always be false and re-trigger the render storm.
                    if prev.team_context.as_deref() == team_context.as_ref()
                        && *prev.inbox == state.inbox
                        && *prev.worker_sandbox_permissions == state.worker_sandbox_permissions
                        && prev.pending_sandbox_request.as_deref()
                            == state.pending_sandbox_request.as_ref()
                    {
                        return crate::state::store::UpdateDecision::Same(());
                    }
                    // P4 review fix (2026-08-02): per-field identity — CC's
                    // per-field spread only replaces the fields it writes, so
                    // an unchanged field keeps its previous reference. Mint a
                    // fresh Arc only for fields whose value changed; the
                    // `(**prev).clone()` baseline already carries prev's Arcs
                    // for the rest.
                    let mut next = (**prev).clone();
                    if prev.team_context.as_deref() != team_context.as_ref() {
                        next.team_context = team_context.clone().map(Arc::new);
                    }
                    if *prev.inbox != state.inbox {
                        next.inbox = Arc::new(state.inbox.clone());
                    }
                    if *prev.worker_sandbox_permissions != state.worker_sandbox_permissions {
                        next.worker_sandbox_permissions =
                            Arc::new(state.worker_sandbox_permissions.clone());
                    }
                    if prev.pending_sandbox_request.as_deref()
                        != state.pending_sandbox_request.as_ref()
                    {
                        next.pending_sandbox_request =
                            state.pending_sandbox_request.clone().map(Arc::new);
                    }
                    crate::state::store::UpdateDecision::Replace {
                        next: Arc::new(next),
                        result: (),
                    }
                });
                inbox_poller_state.set(state.poller);
                let Some(content) = submitted else {
                    return;
                };
                if !pending_responses.read().is_empty()
                    || active_query.read().is_some()
                    || !permission_queue.read().is_empty()
                {
                    return;
                }

                let mcp_state_for_input = runtime_mcp_context
                    .as_ref()
                    .map(|context| context.current())
                    .unwrap_or_default();
                let main_thread_agent_for_input = main_thread_agent_definition.read().clone();
                let input_context = build_repl_process_user_input_context(
                    permission_store.tool_permission_context(),
                    &app_store_for_inbox,
                    history_model_messages(&history_state.read()),
                    resume_restore_stores.read().clone(),
                    loaded_nested_memory_paths.read().clone(),
                    mcp_state_for_input,
                    initial_tools_for_inbox.as_slice(),
                    commands_for_inbox.clone(),
                    debug,
                    &system_prompt_overrides,
                    main_thread_agent_for_input.as_deref(),
                    channel_permission_callbacks_for_inbox.as_ref(),
                    &permission_sink_for_inbox,
                    thinking_config_for_inbox.as_ref(),
                    repl_response_length_sink(response_length_ref_for_inbox),
                );
                // Skipping-hooks seam (TaskList #45): this fires from a
                // synchronous poller callback, so it cannot await the outer
                // `processUserInput`.
                let result = prompt_submit_for_inbox
                    // Inbox-delivered prompts carry no pasted images.
                    .submit_prompt_deferred_query_with_context_skipping_hooks(
                        content,
                        input_context,
                        Vec::new(),
                        Vec::new(),
                    );
                let command_permission_context =
                    apply_repl_command_allowed_tools(&app_store_for_inbox, &result.allowed_tools);
                let Some(mut params) = result.query_params.clone() else {
                    append_prompt_submit_result(&mut history_state, &mut pending_responses, result);
                    let _ = mock_pump_tx_for_inbox.try_send(());
                    return;
                };

                // C3c-4: see the prompt-submit site — the one history feeds
                // both query views by projection.
                append_prompt_submit_result(&mut history_state, &mut pending_responses, result);
                let entries_snapshot = Arc::clone(&*history_state.read());
                let main_thread_agent_definition = main_thread_agent_definition.read().clone();
                let next_content_replacement_state = apply_repl_query_turn_context(
                    &mut params,
                    &app_store_for_inbox,
                    command_permission_context,
                    &entries_snapshot,
                    content_replacement_state.read().clone(),
                    &resume_restore_stores.read(),
                    runtime_mcp_context
                        .as_ref()
                        .map(|context| context.current()),
                    main_thread_agent_definition.as_deref(),
                    initial_tools_for_inbox.as_slice(),
                    &system_prompt_overrides,
                    channel_permission_callbacks_for_inbox.as_ref(),
                    &permission_sink_for_inbox,
                    &has_interruptible_tool_in_progress,
                    thinking_config_for_inbox.as_ref(),
                    // Context came from `build_repl_process_user_input_context`
                    // above, which already applied the builder-owned fields.
                    None,
                );
                if let Some(state) = next_content_replacement_state {
                    content_replacement_state.set(state);
                }

                stream_mode.set(SpinnerMode::Responding);
                response_length_ref.set(0);
                active_query.set(Some(spawn_query(params, production_deps())));
                let _ = mock_pump_tx_for_inbox.try_send(());
            }
        },
        inbox_poll_interval,
    );

    // Maps to: CC `useScheduledTasks` 1s tick + `useQueueProcessor` idle drain.
    hooks.use_interval(
        {
            let cron_scheduler = cron_scheduler.clone();
            let prompt_submit_for_cron = prompt_submit.clone();
            let prompt_shell_sender_for_cron = prompt_shell_channel.0.clone();
            let mock_pump_tx_for_cron = mock_pump_tx.clone();
            let mut history_state = history_state;
            let mut pending_responses = pending_responses;
            let mut active_query = active_query;
            let mut permission_queue = permission_queue;
            let permission_store = permission_store.clone();
            let mut content_replacement_state = content_replacement_state;
            let mut resume_restore_stores = resume_restore_stores;
            let mut loaded_nested_memory_paths = loaded_nested_memory_paths;
            let main_thread_agent_definition = main_thread_agent_definition;
            let system_prompt_overrides = system_prompt_overrides.clone();
            let runtime_mcp_context = runtime_mcp_context.clone();
            let channel_permission_callbacks_for_cron = channel_permission_callbacks.clone();
            let permission_sink_for_cron = interactive_permission_sink.clone();
            let app_store_for_cron = app_store.clone();
            let thinking_config_for_cron = thinking_config.clone();
            let initial_tools_for_cron = initial_tools.clone();
            let commands_for_cron = commands.clone();
            let initial_main_thread_agent_definition_for_cron =
                initial_main_thread_agent_definition.clone();
            let resume_processing_tx_for_cron = resume_processing_tx.clone();
            let context_processing_tx_for_cron = context_processing_tx.clone();
            let plan_processing_tx_for_cron = plan_processing_tx.clone();
            let keybindings_processing_tx_for_cron = keybindings_processing_tx.clone();
            let compact_processing_tx_for_cron = compact_processing_tx.clone();
            let mut active_compact_abort_for_cron = active_compact_abort;
            let advisor_action_for_cron = advisor_action.clone();
            let reload_plugins_action_for_cron = reload_plugins_action.clone();
            let export_action_for_cron = export_action.clone();
            let terminal_setup_action_for_cron = terminal_setup_action.clone();
            let color_action_for_cron = color_action.clone();
            let copy_direct_action_for_cron = copy_direct_action.clone();
            let rename_generation_action_for_cron = rename_generation_action.clone();
            let terminal_cols_for_cron = terminal_cols;
            let mut stream_mode = stream_mode;
            let mut response_length_ref = response_length_ref;
            let mut active_local_command_ui = active_local_command_ui;
            let mut message_selector_visible = message_selector_visible;
            let mut exit_flow_active = exit_flow_active;
            let mut active_prompt_shell_for_cron = active_prompt_shell_command;
            let has_interruptible_tool_in_progress =
                Arc::clone(&has_interruptible_tool_in_progress);
            move || {
                // Maps to: CC `useScheduledTasks` — cron only. The queue drain
                // below is CC `useQueueProcessor`, a SEPARATE effect with no
                // cron gate (`useQueueProcessor.ts:47-52` guards only on
                // isQueryActive / hasActiveLocalJsxUI / queue length). Both
                // share this one interval in the port, so the cron gate must
                // not swallow the drain: while it did, a background agent that
                // finished with the REPL idle left its `task-notification`
                // queued forever — no `⏺ Agent "…" completed` row, and
                // `showSpinner`'s `commandQueueLength > 0` branch
                // (`REPL.tsx:2245-2263`) spun for the rest of the session.
                if crate::hooks::use_scheduled_tasks::should_run_scheduled_tasks() {
                    let is_loading_now = !pending_responses.read().is_empty()
                        || active_query.read().is_some()
                        || !permission_queue.read().is_empty();

                    let fired = match cron_scheduler.lock() {
                        Ok(mut scheduler) => scheduler.on_tick(is_loading_now),
                        Err(_) => Vec::new(),
                    };
                    for task in fired {
                        if let Some(text) =
                            crate::hooks::use_scheduled_tasks::handle_fired_task_for_lead(&task)
                        {
                            // Maps to: CC `useScheduledTasks.ts:110-114`
                            // `createScheduledTaskFireMessage(...)` +
                            // `setMessages(prev => [...prev, msg])` — the fire
                            // notice is a `Message` member of the one history
                            // (batch D3); the API projection drops system members.
                            let mut entries = history_state.read().as_ref().clone();
                            entries.push(HistoryEntry::Message(Message::System(
                                SystemMessage::ScheduledTaskFire {
                                    base: crate::types::message::SystemBase::new(),
                                    content: text,
                                },
                            )));
                            history_state.set(Arc::new(entries));
                        }
                    }
                }

                // Maps to: CC `useQueueProcessor` / `processQueueIfReady` — drain
                // main-thread queued commands when idle (cron fires and task
                // notifications use `later`).
                let still_loading = !pending_responses.read().is_empty()
                    || active_query.read().is_some()
                    || !permission_queue.read().is_empty()
                    || active_local_command_ui.read().is_some()
                    || active_prompt_shell_for_cron.read().is_some();
                if still_loading {
                    return;
                }
                let Some(queued_commands) = crate::utils::queue_processor::process_queue_if_ready()
                else {
                    return;
                };
                let next = queued_commands
                    .first()
                    .expect("queue processor returns a non-empty group");
                if next.mode == "bash" {
                    let command = queued_commands
                        .into_iter()
                        .next()
                        .expect("bash queue command");
                    active_prompt_shell_for_cron.set(Some(command.value.clone()));
                    let sender = prompt_shell_sender_for_cron.clone();
                    let shell_store = app_store_for_cron.clone();
                    std::thread::spawn(move || {
                        let args = serde_json::json!({
                            "command": command.value,
                            "dangerouslyDisableSandbox": true,
                        });
                        let context =
                            crate::tool::ToolUseContext::default().with_app_store(shell_store);
                        let result =
                            crate::tools::bash_tool::bash_output(&args, &context, None, None);
                        let command = args
                            .get("command")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let _ = sender.send_blocking((command, result));
                    });
                    return;
                }
                if next.mode != "prompt" && next.mode != "task-notification" {
                    return;
                }
                let mcp_state_for_input = runtime_mcp_context
                    .as_ref()
                    .map(|context| context.current())
                    .unwrap_or_default();
                let main_thread_agent_for_input = main_thread_agent_definition.read().clone();
                let input_context = build_repl_process_user_input_context(
                    permission_store.tool_permission_context(),
                    &app_store_for_cron,
                    history_model_messages(&history_state.read()),
                    resume_restore_stores.read().clone(),
                    loaded_nested_memory_paths.read().clone(),
                    mcp_state_for_input,
                    initial_tools_for_cron.as_slice(),
                    commands_for_cron.clone(),
                    debug,
                    &system_prompt_overrides,
                    main_thread_agent_for_input.as_deref(),
                    channel_permission_callbacks_for_cron.as_ref(),
                    &permission_sink_for_cron,
                    thinking_config_for_cron.as_ref(),
                    repl_response_length_sink(response_length_ref),
                );
                // CC routes slash-shaped queued input through the same
                // `handlePromptSubmit({ queuedCommands })` owner as prompts.
                // The queue processor isolates it; this call still preserves
                // `skipSlashCommands`, pasted image metadata, UUIDs, and
                // `isMeta` instead of redispatching through a second shortcut.
                let result = prompt_submit_for_cron
                    .submit_queued_commands_deferred_query_with_context(
                        queued_commands,
                        input_context,
                    );
                // Maps to CC `executeUserInput`'s local-command completion
                // branch. `processUserInputBase` returns the action envelope;
                // the REPL owns the stateful UI side effect, even when the
                // command arrived through the queue rather than the keyboard.
                let is_clear_conversation = matches!(
                    result.local_action,
                    Some(
                        crate::utils::process_user_input::process_slash_command::SlashCommandAction::ClearConversation,
                    )
                );
                if is_clear_conversation {
                    // Keep `/clear`'s owner and ordering identical to the
                    // direct submit path: clear the one history projection,
                    // restore stores, pending responses, and session state;
                    // then retain only the command/session-start rows.
                    pending_responses.set(Vec::new());
                    if let Some(handle) = active_query.read().clone() {
                        let _ = handle.commands.try_send(QueryCommand::Abort);
                        handle.abort_controller.abort();
                    }
                    active_query.set(None);
                    permission_queue.set(Vec::new());
                    resume_restore_stores.set(ResumeRestoreStores::default());
                    loaded_nested_memory_paths.set(std::collections::HashSet::new());
                    let clear_outcome =
                        crate::commands::clear::clear_conversation(&app_store_for_cron);
                    let mut cleared_messages = result.messages.clone();
                    cleared_messages.extend(clear_outcome.session_start_messages);
                    history_state.set(Arc::new(
                        cleared_messages
                            .into_iter()
                            .map(history_entry_from_row)
                            .collect::<Vec<_>>(),
                    ));
                    content_replacement_state
                        .set(crate::utils::tool_result_storage::ContentReplacementState::new());
                }
                match result.local_action.clone() {
                    Some(SlashCommandAction::Advisor {
                        args,
                        context,
                        invocation,
                        user_message,
                    }) => {
                        advisor_action_for_cron((args, context, invocation, user_message));
                    }
                    Some(SlashCommandAction::ReloadPlugins { user_message, .. }) => {
                        reload_plugins_action_for_cron(user_message);
                    }
                    Some(SlashCommandAction::ExportConversation {
                        preceding_input_blocks,
                        args,
                        context,
                        invocation,
                    }) => {
                        export_action_for_cron((args, context, invocation, preceding_input_blocks));
                    }
                    Some(SlashCommandAction::SetupTerminal { invocation }) => {
                        terminal_setup_action_for_cron(invocation);
                    }
                    Some(
                        crate::utils::process_user_input::process_slash_command::SlashCommandAction::OpenLocalCommandUi {
                            command,
                            invocation,
                        },
                    ) => {
                        let current = active_local_command_ui.read().clone();
                        active_local_command_ui.set(set_local_command_ui(
                            current,
                            ActiveLocalCommandUi::from_slash_command_with_source(
                                command, invocation, false, false,
                            ),
                        ));
                    }
                    Some(SlashCommandAction::BranchConversation { args, invocation }) => {
                        let invocation = LocalCommandInvocation {
                            slash_command: invocation,
                            dismiss_result: None,
                        };
                        let app_snapshot = app_store_for_cron.get();
                        let agent_type = main_thread_agent_definition
                            .read()
                            .as_ref()
                            .map(|agent| agent.agent_type.clone());
                        let model = Some(repl_main_loop_model(&app_snapshot));
                        let initial_agent = initial_main_thread_agent_definition_for_cron.clone();
                        let agent_definitions = app_snapshot.agent_definitions.clone();
                        let sender = resume_processing_tx_for_cron.clone();
                        std::thread::spawn(move || {
                            let prepared = crate::commands::branch::branch::call(&args);
                            let (success_message, result) = match prepared {
                                Ok(prepared) => (
                                    Some(prepared.success_message),
                                    resume(
                                        prepared.target,
                                        initial_agent,
                                        agent_definitions,
                                        agent_type,
                                        model,
                                    ),
                                ),
                                Err(error) => (None, Err(error)),
                            };
                            let result = result
                                .map_err(|error| format!("Failed to branch conversation: {error}"));
                            let _ = sender.send_blocking((
                                Some(invocation),
                                success_message,
                                result,
                                None,
                            ));
                        });
                    }
                    Some(SlashCommandAction::ResumeByArg { arg, invocation }) => {
                        let invocation = LocalCommandInvocation {
                            slash_command: invocation,
                            dismiss_result: None,
                        };
                        let app_snapshot = app_store_for_cron.get();
                        let agent_type = main_thread_agent_definition
                            .read()
                            .as_ref()
                            .map(|agent| agent.agent_type.clone());
                        let model = Some(repl_main_loop_model(&app_snapshot));
                        let initial_agent = initial_main_thread_agent_definition_for_cron.clone();
                        let agent_definitions = app_snapshot.agent_definitions.clone();
                        let sender = resume_processing_tx_for_cron.clone();
                        let project_path = crate::bootstrap::state::get_original_cwd()
                            .display()
                            .to_string();
                        std::thread::spawn(move || {
                            let result = crate::commands::resume::load_for_arg(&project_path, &arg)
                                .map_err(|error| error.to_string())
                                .and_then(|target| {
                                    resume(
                                        target,
                                        initial_agent,
                                        agent_definitions,
                                        agent_type,
                                        model,
                                    )
                                });
                            let _ = sender.send_blocking((Some(invocation), None, result, None));
                        });
                    }
                    Some(SlashCommandAction::SetSessionColor {
                        args,
                        context,
                        invocation,
                    }) => {
                        color_action_for_cron((args, context, invocation, false));
                    }
                    Some(SlashCommandAction::CopyToClipboard {
                        content,
                        invocation,
                    }) => {
                        copy_direct_action_for_cron((content, invocation));
                    }
                    Some(SlashCommandAction::GenerateSessionName { request, invocation }) => {
                        rename_generation_action_for_cron((request, invocation));
                    }
                    Some(SlashCommandAction::AnalyzeContext {
                        mut request,
                        invocation,
                    }) => {
                        request.terminal_width = Some(terminal_cols_for_cron.max(1));
                        request.system_prompt_overrides = system_prompt_overrides.clone();
                        request.main_thread_agent_definition = main_thread_agent_definition
                            .read()
                            .clone()
                            .map(Arc::unwrap_or_clone);
                        let sender = context_processing_tx_for_cron.clone();
                        std::thread::spawn(move || {
                            let result = crate::commands::context::context::call(&request);
                            let _ = sender.send_blocking((invocation, result));
                        });
                    }
                    Some(SlashCommandAction::CompactConversation {
                        custom_instructions,
                        context,
                        invocation,
                    }) => {
                        let context = Arc::unwrap_or_clone(context);
                        let messages = context.messages.clone();
                        let main_thread_agent = main_thread_agent_definition.read().clone();
                        let prompt_overrides = system_prompt_overrides.clone();
                        let abort_controller = context.abort_controller.clone();
                        active_compact_abort_for_cron.set(Some(abort_controller));
                        stream_mode.set(SpinnerMode::Requesting);
                        response_length_ref.set(0);
                        let sender = compact_processing_tx_for_cron.clone();
                        let worker_sender = sender.clone();
                        let worker_invocation = invocation.clone();
                        let spawn_result = std::thread::Builder::new()
                            .name("cometix-compact".to_string())
                            .stack_size(8 * 1024 * 1024)
                            .spawn(move || {
                                let result = match tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                {
                                    Ok(runtime) => runtime.block_on(async {
                                        crate::commands::compact::compact::call(
                                            messages,
                                            &context,
                                            || {
                                                let model = context.main_loop_model.clone().unwrap_or_else(
                                                    crate::utils::model::model::get_main_loop_model,
                                                );
                                                let additional_working_directories: Vec<String> = context
                                                    .get_app_state()
                                                    .map(|state| {
                                                        state
                                                            .tool_permission_context
                                                            .additional_working_directories
                                                            .keys()
                                                            .cloned()
                                                            .collect()
                                                    })
                                                    .unwrap_or_else(|| {
                                                        context
                                                            .tool_permission_context
                                                            .additional_working_directories
                                                            .keys()
                                                            .cloned()
                                                            .collect()
                                                    });
                                                let (system_prompt, user_context, system_context) =
                                                    build_repl_query_prompt_contexts(
                                                        &prompt_overrides,
                                                        crate::constants::prompts::get_system_prompt(
                                                            &context.tools,
                                                            &model,
                                                            &additional_working_directories,
                                                            &context.mcp_state.clients,
                                                        ),
                                                        main_thread_agent.as_deref(),
                                                        &context.mcp_state,
                                                    );
                                                crate::commands::compact::compact::CompactCommandCacheContext {
                                                    system_prompt,
                                                    user_context,
                                                    system_context,
                                                }
                                            },
                                            &custom_instructions,
                                        )
                                        .await
                                        .map_err(|error| {
                                            if context.abort_controller.is_aborted() {
                                                "Compaction canceled".to_string()
                                            } else {
                                                error
                                            }
                                        })
                                    }),
                                    Err(error) => Err(format!(
                                        "Failed to start compact runtime: {error}"
                                    )),
                                };
                                let _ = worker_sender.send_blocking((worker_invocation, result));
                            });
                        if let Err(error) = spawn_result {
                            let _ = sender.send_blocking((
                                invocation,
                                Err(format!("Failed to start compact worker: {error}")),
                            ));
                        }
                    }
                    Some(SlashCommandAction::InspectPlan { request, invocation }) => {
                        let sender = plan_processing_tx_for_cron.clone();
                        std::thread::spawn(move || {
                            let result = crate::commands::plan::plan::inspect_plan(&request);
                            let _ = sender.send_blocking((invocation, result));
                        });
                    }
                    Some(SlashCommandAction::EditKeybindings { invocation }) => {
                        let sender = keybindings_processing_tx_for_cron.clone();
                        std::thread::spawn(move || {
                            let result = crate::commands::keybindings::keybindings::prepare_keybindings_file();
                            let _ = sender.send_blocking((invocation, result));
                        });
                    }
                    Some(SlashCommandAction::ApplyPlanMode { .. }) => {
                        // The command callback already committed the AppStore
                        // update; the query context carries the new mode.
                    }
                    Some(
                        crate::utils::process_user_input::process_slash_command::SlashCommandAction::ClearConversation,
                    ) if !is_clear_conversation => {
                        let current = active_local_command_ui.read().clone();
                        active_local_command_ui.set(clear_local_command_ui(current));
                    }
                    Some(
                        crate::utils::process_user_input::process_slash_command::SlashCommandAction::OpenMessageSelector,
                    ) => {
                        message_selector_visible.set(true);
                    }
                    Some(
                        crate::utils::process_user_input::process_slash_command::SlashCommandAction::Exit,
                    ) => {
                        let current = active_local_command_ui.read().clone();
                        active_local_command_ui.set(clear_local_command_ui(current));
                        exit_flow_active.set(true);
                    }
                    _ => {}
                }
                if is_clear_conversation {
                    let _ = mock_pump_tx_for_cron.try_send(());
                    return;
                }
                let command_permission_context =
                    apply_repl_command_allowed_tools(&app_store_for_cron, &result.allowed_tools);
                let Some(mut params) = result.query_params.clone() else {
                    append_prompt_submit_result(
                        &mut history_state,
                        &mut pending_responses,
                        result.clone(),
                    );
                    let _ = mock_pump_tx_for_cron.try_send(());
                    return;
                };

                // C3c-4: see the prompt-submit site — the one history feeds
                // both query views by projection.
                append_prompt_submit_result(
                    &mut history_state,
                    &mut pending_responses,
                    result.clone(),
                );
                let entries_snapshot = Arc::clone(&*history_state.read());
                let main_thread_agent_definition = main_thread_agent_definition.read().clone();
                let next_content_replacement_state = apply_repl_query_turn_context(
                    &mut params,
                    &app_store_for_cron,
                    command_permission_context,
                    &entries_snapshot,
                    content_replacement_state.read().clone(),
                    &resume_restore_stores.read(),
                    runtime_mcp_context
                        .as_ref()
                        .map(|context| context.current()),
                    main_thread_agent_definition.as_deref(),
                    initial_tools_for_cron.as_slice(),
                    &system_prompt_overrides,
                    channel_permission_callbacks_for_cron.as_ref(),
                    &permission_sink_for_cron,
                    &has_interruptible_tool_in_progress,
                    thinking_config_for_cron.as_ref(),
                    // Context came from `build_repl_process_user_input_context`
                    // above, which already applied the builder-owned fields.
                    None,
                );
                if let Some(state) = next_content_replacement_state {
                    content_replacement_state.set(state);
                }

                stream_mode.set(SpinnerMode::Responding);
                response_length_ref.set(0);
                active_query.set(Some(spawn_query(params, production_deps())));
                let _ = mock_pump_tx_for_cron.try_send(());
            }
        },
        Some(Duration::from_millis(
            crate::hooks::use_scheduled_tasks::check_interval_ms(),
        )),
    );

    // Maps to: CC PromptInput → onExit → REPL → exit()
    let on_exit = move |_: ()| {
        if let Some(handle) = active_query.read().clone() {
            // Maps to CC `REPL.tsx` interrupt path preserving partial
            // `streamingText` before clearing loading state.
            append_partial_streaming_text_message(
                &mut history_state,
                streaming_text_preview.read().clone(),
            );
            streaming_text_preview.set(None);
            let _ = handle.commands.try_send(QueryCommand::Abort);
            handle.abort_controller.abort();
        }
        active_query.set(None);
        permission_queue.set(Vec::new());
        let current = active_local_command_ui.read().clone();
        active_local_command_ui.set(clear_local_command_ui(current));
        // Maps to: CC REPL.tsx:4884-4885 handleExit non-worktree path —
        // `await exitMod.call(() => {})`: the no-op onDone drops the goodbye
        // and gracefulShutdown exits directly; no ExitFlow mounts. The
        // worktree branch (setExitFlow(<ExitFlow showWorktree .../>)) joins
        // when `get_current_worktree_session()` is Some.
        let mut should_exit = should_exit;
        should_exit.set(true);
    };

    let mut on_local_command_ui_close = move |_: ()| {
        if let Some(active) = active_local_command_ui.read().clone() {
            append_local_command_ui_dismiss_result(&mut history_state, &active);
        }
        let current = active_local_command_ui.read().clone();
        active_local_command_ui.set(clear_local_command_ui(current));
    };

    // Maps to processSlashCommand.tsx:733 onward: each local-jsx Promise
    // captures its invocation. A late completion never reads a newer panel's
    // invocation or settles it. Only the clipboard-capable owners need an
    // owned Handler because their source async work survives child unmount.
    #[cfg(test)]
    let clipboard_callback_observer = hooks
        .try_use_context::<tests::ClipboardCallbackObserver>()
        .map(|observer| observer.clone());
    let on_clipboard_command_result = move |active: ActiveLocalCommandUi| {
        #[cfg(test)]
        let clipboard_callback_observer = clipboard_callback_observer.clone();
        Handler::from(move |output: String| {
            #[cfg(test)]
            let _callback_return = clipboard_callback_observer
                .as_ref()
                .and_then(|observer| observer.observe(&output));
            let mut history_state = history_state;
            let mut active_local_command_ui = active_local_command_ui;
            if active.completion.set(()).is_err() {
                return;
            }
            if let Some(invocation) = active.invocation.as_ref() {
                let rows = if let LocalCommandPanel::Export(_, preceding) = &active.panel {
                    crate::utils::process_user_input::process_slash_command::local_jsx_command_result(
                        &invocation.slash_command.command_name, &invocation.slash_command.args,
                        Some(&output), None, false, &[], preceding.clone()).messages
                } else {
                    local_command_invocation_result_messages(invocation, &output, false)
                };
                append_history_rows(&mut history_state, rows);
            }
            if let Some(mut current) = active_local_command_ui.try_write() {
                if current
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(&current.completion, &active.completion))
                {
                    *current = None;
                }
            }
        })
    };

    let mut on_local_command_ui_result = move |output: String| {
        if let Some(active) = active_local_command_ui.read().clone() {
            if let Some(invocation) = active.invocation.as_ref() {
                if let LocalCommandPanel::Export(_, preceding_input_blocks) = &active.panel {
                    let result = crate::utils::process_user_input::process_slash_command::local_jsx_command_result(
                        &invocation.slash_command.command_name, &invocation.slash_command.args, Some(&output), None,
                        false, &[], preceding_input_blocks.clone());
                    append_history_rows(&mut history_state, result.messages);
                } else if matches!(active.panel, LocalCommandPanel::AddDir { .. }) {
                    let mut result = crate::utils::process_user_input::process_slash_command::model_visible_local_command_result(None, &invocation.slash_command.command_name, &invocation.slash_command.args, &output, false, None);
                    crate::utils::process_user_input::process_slash_command::prepend_local_command_caveat(&mut result);
                    append_history_rows(&mut history_state, result.messages);
                } else {
                    append_local_command_invocation_result(
                        &mut history_state,
                        invocation,
                        &output,
                        false,
                    );
                }
            }
        }
        let current = active_local_command_ui.read().clone();
        active_local_command_ui.set(clear_local_command_ui(current));
    };

    // CC commands/permissions/permissions.tsx:10-16 calls context.setMessages
    // before onDone. Keep this system member in the typed session ledger;
    // API normalization, not display-row transport, filters it for the model.
    let on_permissions_message = move |message: Message| {
        let mut entries = history_state.read().as_ref().clone();
        entries.push(HistoryEntry::Message(message));
        history_state.set(Arc::new(entries));
    };
    let permission_notification_store = app_store.clone();
    let permission_query_action = prompt_query_action.clone();
    let permission_mcp_context = runtime_mcp_context.clone();
    let on_permissions_done = move |done: crate::components::permissions::rules::permission_rule_list::PermissionRuleListExit| {
        use crate::utils::worktree::CommandResultDisplay;
        let current = active_local_command_ui.read().clone();
        let continuation = permissions_continuation.read().clone();
        permissions_continuation.set(None);
        active_local_command_ui.set(clear_local_command_ui(current.clone()));
        if let Some(active) = current {
            if let Some(invocation) = active.invocation.as_ref() {
                let command = &invocation.slash_command.command_name;
                let args = &invocation.slash_command.args;
                if active.is_immediate {
                    // CC REPL.tsx:4346-4388: shouldQuery is intentionally not
                    // read here. Meta messages still append even for display:skip.
                    if let Some(output) = done.result.as_deref().filter(|text| !text.is_empty()) {
                        if done.display != Some(CommandResultDisplay::Skip) {
                            let mut notifications = crate::context::notifications::NotificationsWriter::new(permission_notification_store.clone());
                            notifications.add_notification(crate::context::notifications::Notification::text(
                                format!("immediate-{command}"), output.to_string(),
                                crate::context::notifications::NotificationPriority::Immediate,
                            ));
                            if !crate::utils::fullscreen::is_fullscreen_env_enabled() {
                                append_history_rows(&mut history_state, local_command_result_messages(
                                    command, args, &crate::utils::xml::escape_xml(output), false,
                                ));
                            }
                        }
                    }
                    let mut entries = history_state.read().as_ref().clone();
                    entries.extend(done.meta_messages.into_iter().map(|text| HistoryEntry::Message(Message::User(
                        crate::utils::messages::create_user_message_with_meta(text, true),
                    ))));
                    history_state.set(Arc::new(entries));
                } else {
                    let result = crate::utils::process_user_input::process_slash_command::local_jsx_command_result(
                        command, args, done.result.as_deref(), done.display, done.should_query, &done.meta_messages,
                        continuation.as_ref().map(|value| value.preceding_input_blocks.clone()).unwrap_or_default(),
                    );
                    if let Some(continuation) = continuation {
                        // Resume the awaited processUserInput result, including
                        // its single UserPromptSubmit hook tail. This reuses the
                        // existing REPL query/context pipeline without dispatching
                        // /permissions a second time.
                        permission_query_action(PromptQuerySubmit {
                            text: continuation.input.clone(),
                            image_contents: Vec::new(), image_paste_ids: Vec::new(),
                            from_keybinding: false, is_query_active_for_immediate: false,
                            tool_permission_context: permission_notification_store.tool_permission_context(),
                            runtime_mcp_state: permission_mcp_context.as_ref().map(|context| context.current()).unwrap_or_default(),
                            completed_local_command: Some(CompletedLocalCommand { continuation, processed: result }),
                        });
                    } else {
                        // Only non-query fixture panels can lack a suspended
                        // invocation; every production OpenLocalCommandUi stores it.
                        debug_assert!(!result.should_query, "permissions retry requires its dispatch context");
                        append_history_rows(&mut history_state, result.messages);
                    }
                }
            }
        }
    };

    let mut on_diff_done = move |done: DiffDialogDone| {
        if done.display != crate::utils::worktree::CommandResultDisplay::Skip {
            if let Some(active) = active_local_command_ui.read().clone() {
                if let Some(invocation) = active.invocation.as_ref() {
                    append_local_command_invocation_result(
                        &mut history_state,
                        invocation,
                        &done.result,
                        false,
                    );
                }
            }
        }
        let current = active_local_command_ui.read().clone();
        active_local_command_ui.set(clear_local_command_ui(current));
    };

    let mut on_hooks_done = move |done: HooksConfigMenuDone| {
        if done.display != crate::utils::worktree::CommandResultDisplay::Skip {
            if let Some(active) = active_local_command_ui.read().clone() {
                if let Some(invocation) = active.invocation.as_ref() {
                    append_local_command_invocation_result(
                        &mut history_state,
                        invocation,
                        &done.result,
                        false,
                    );
                }
            }
        }
        let current = active_local_command_ui.read().clone();
        active_local_command_ui.set(clear_local_command_ui(current));
    };

    let mut on_agents_done = move |result: String| {
        if let Some(active) = active_local_command_ui.read().clone() {
            if let Some(invocation) = active.invocation.as_ref() {
                append_local_command_invocation_result(
                    &mut history_state,
                    invocation,
                    &result,
                    false,
                );
            }
        }
        let current = active_local_command_ui.read().clone();
        active_local_command_ui.set(clear_local_command_ui(current));
    };

    let mut on_skills_done = move |done: SkillsMenuDone| {
        if done.display != crate::utils::worktree::CommandResultDisplay::Skip {
            if let Some(active) = active_local_command_ui.read().clone() {
                if let Some(invocation) = active.invocation.as_ref() {
                    append_local_command_invocation_result(
                        &mut history_state,
                        invocation,
                        &done.result,
                        false,
                    );
                }
            }
        }
        let current = active_local_command_ui.read().clone();
        active_local_command_ui.set(clear_local_command_ui(current));
    };

    let app_store_for_resume = app_store.clone();
    let initial_main_thread_agent_definition_for_resume =
        initial_main_thread_agent_definition.clone();
    let main_thread_agent_definition_for_resume = main_thread_agent_definition;
    let resume_processing_tx_for_picker = resume_processing_tx.clone();
    let on_resume_select = move |active: ActiveLocalCommandUi, selection: SessionSelection| {
        let app_snapshot = app_store_for_resume.get();
        let agent_type = main_thread_agent_definition_for_resume
            .read()
            .as_ref()
            .map(|agent| agent.agent_type.clone());
        let model = Some(repl_main_loop_model(&app_snapshot));
        let initial_agent = initial_main_thread_agent_definition_for_resume.clone();
        let agent_definitions = app_snapshot.agent_definitions.clone();
        let sender = resume_processing_tx_for_picker.clone();
        std::thread::spawn(move || {
            let result = resume::load_for_picker_selection(&selection)
                .map_err(|error| error.to_string())
                .and_then(|target| {
                    resume(target, initial_agent, agent_definitions, agent_type, model)
                });
            let _ = sender.send_blocking((None, None, result, Some(active)));
        });
    };

    let mock_pump_tx_for_permission_select = mock_pump_tx.clone();
    let permission_context_for_response = permission_store.clone();
    let app_store_for_permission_response = app_store.clone();
    let runtime_mcp_context_for_response = runtime_mcp_context.clone();
    let confirm_updaters_for_response = tool_use_confirm_channel.1.clone();
    let on_permission_response = move |response: PermissionPromptResponse| {
        let mut permission_queue_state = permission_queue;
        let active_query_state = active_query;
        let permission_context_state = permission_context_for_response.clone();
        let mut pending_responses_state = pending_responses;
        let mut history_state_for_permission = history_state;
        let previous = permission_queue_state.read().clone();
        let mut queue = previous.clone();
        // Settle the FIFO, address the row, claim it. All three inside one
        // function so the order holds — see `claim_permission_answer_from_queue`.
        let answered = claim_permission_answer_from_queue(
            &confirm_updaters_for_response,
            &mut queue,
            response,
            &app_store_for_permission_response,
        );
        // React bails out when a state update hands back what it was given;
        // settling with nothing pending and no row taken is exactly that case.
        if queue != previous {
            permission_queue_state.set(queue);
        }
        let Some((confirm, response)) = answered else {
            return;
        };
        let mailbox_response_target = confirm.mailbox_response_target.clone();
        let responder = confirm.responder.clone();
        let request = confirm.request.clone();
        let effective_request = response.apply_to_request(request.clone());
        let choice = response.choice;

        if let Some(target) = mailbox_response_target {
            let _ = send_mailbox_permission_prompt_response(&target, &response);
            let _ = mock_pump_tx_for_permission_select.try_send(());
            return;
        }

        // CC resolves the answer through the entry's own `onAllow`/`onReject`
        // (`PermissionRequest.tsx:158-164`) — always, for the leader's own
        // tools as much as for a nested query's — while
        // `ctx.setToolPermissionContext` applies the grant to the parent's
        // context (`hooks/toolPermission/PermissionContext.ts:139-147`). Both
        // resolver transports land here: a nested query's channel and the
        // REPL's own `QueryCommand::PermissionResponse` sink, which is bound to
        // the handle that RAISED the row rather than to whatever
        // `active_query` names now.
        //
        // The one row whose write-back is NOT that plain
        // `PermissionContext.ts:139-147` call is an in-process teammate's: CC
        // `inProcessRunner.ts:263-281` hands the leader
        // `{ preserveMode: true }` so a worker's transformed context cannot
        // change the coordinator's mode (a `setMode` update in the answer —
        // ExitPlanMode's "auto-accept edits", say — would otherwise move the
        // LEADER into acceptEdits because a teammate was asked). Which of the
        // two runs is decided inside
        // `apply_permission_answer_to_leader_context`, off the row's builder,
        // and it must run AFTER `claim_permission_answer` has done the disk
        // write and BEFORE `respond` — see both functions' contracts.
        //
        // The `active_query` arm below is what remains for a row with no
        // resolver: the scripted `pending_responses` runtime's, which queues
        // rows through `run_tools_for_message` with no asking actor behind
        // them.
        if responder.is_some() {
            apply_permission_answer_to_leader_context(
                &confirm,
                &permission_context_state,
                &effective_request,
                &response,
            );
            responder.respond(response);
            let _ = mock_pump_tx_for_permission_select.try_send(());
            return;
        }

        if let Some(handle) = active_query_state.read().clone() {
            let (next_context, _decision) = apply_prompt_response(
                &permission_context_state.tool_permission_context(),
                &effective_request,
                &response,
            );
            permission_context_state.set_tool_permission_context(next_context);
            let _ = handle.commands.try_send(QueryCommand::PermissionResponse {
                tool_use_id: request.tool_use_id.clone(),
                response,
            });
        } else {
            if !matches!(
                choice,
                PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
            ) {
                pending_responses_state.set(Vec::new());
            }

            let context = ToolUseContext::with_permission_context(
                permission_context_state.tool_permission_context(),
            )
            .with_app_store(permission_context_state.clone())
            .with_mcp_state(
                runtime_mcp_context_for_response
                    .as_ref()
                    .map(|context| context.current())
                    .unwrap_or_default(),
            );
            let result = check_permissions_and_call_tool_with_response(
                &effective_request,
                &response,
                &context,
                None,
            );
            permission_context_state
                .set_tool_permission_context(result.new_context.tool_permission_context);

            // Model-visibility by type: the tool_result user row becomes
            // a `Message` entry (CC's single array holds tool results), while
            // any other display row stays render-only.
            if let Some(tool_result) = result.message {
                append_history_rows(&mut history_state_for_permission, [tool_result]);
            }
        }
        let _ = mock_pump_tx_for_permission_select.try_send(());
    };

    let mock_pump_tx_for_permission_cancel = mock_pump_tx.clone();
    let permission_context_for_cancel = permission_store.clone();
    let runtime_mcp_context_for_cancel = runtime_mcp_context.clone();
    let confirm_updaters_for_cancel = tool_use_confirm_channel.1.clone();
    let on_permission_cancel = move |tool_use_id: String| {
        let mut permission_queue_state = permission_queue;
        let active_query_state = active_query;
        let permission_context_state = permission_context_for_cancel.clone();
        let mut pending_responses_state = pending_responses;
        let mut history_state_for_permission = history_state;
        let previous = permission_queue_state.read().clone();
        let mut queue = previous.clone();
        // Same settle-then-address-then-claim order as the allow path.
        let cancelled = claim_permission_cancel_from_queue(
            &confirm_updaters_for_cancel,
            &mut queue,
            (!tool_use_id.is_empty()).then_some(tool_use_id.as_str()),
        );
        if queue != previous {
            permission_queue_state.set(queue);
        }
        let Some(confirm) = cancelled else {
            return;
        };
        let mailbox_response_target = confirm.mailbox_response_target.clone();
        let responder = confirm.responder.clone();
        let request = confirm.request;

        // CC `FallbackPermissionRequest.tsx:108-120` `handleCancel` (Esc) and
        // `PermissionRequest.tsx:206-214` (Ctrl-C) both call
        // `toolUseConfirm.onReject()` → `cancelAndAbort`. There is no Cancel
        // choice. This used to emit `PermissionPromptChoice::Cancel`, which
        // skipped `cancel_and_abort` (`tool_execution.rs:8298`) and so did not
        // abort the turn — CC's main-loop Esc/No both do
        // (`PermissionContext.ts:166-171`). Deny, with no decision_message,
        // is the dialog-reject path.
        let deny = PermissionPromptResponse::new(PermissionPromptChoice::Deny);

        if let Some(target) = mailbox_response_target {
            let _ = send_mailbox_permission_prompt_response(&target, &deny);
            let _ = mock_pump_tx_for_permission_cancel.try_send(());
            return;
        }

        if responder.is_some() {
            let (next_context, _decision) = apply_prompt_choice(
                &permission_context_state.tool_permission_context(),
                &request,
                PermissionPromptChoice::Deny,
            );
            permission_context_state.set_tool_permission_context(next_context);
            responder.respond(deny);
            let _ = mock_pump_tx_for_permission_cancel.try_send(());
            return;
        }

        if let Some(handle) = active_query_state.read().clone() {
            let (next_context, _decision) = apply_prompt_choice(
                &permission_context_state.tool_permission_context(),
                &request,
                PermissionPromptChoice::Deny,
            );
            permission_context_state.set_tool_permission_context(next_context);
            let _ = handle.commands.try_send(QueryCommand::PermissionResponse {
                tool_use_id: request.tool_use_id.clone(),
                response: deny,
            });
        } else {
            pending_responses_state.set(Vec::new());

            let context = ToolUseContext::with_permission_context(
                permission_context_state.tool_permission_context(),
            )
            .with_app_store(permission_context_state.clone())
            .with_mcp_state(
                runtime_mcp_context_for_cancel
                    .as_ref()
                    .map(|context| context.current())
                    .unwrap_or_default(),
            );
            let result =
                check_permissions_and_call_tool_with_response(&request, &deny, &context, None);
            permission_context_state
                .set_tool_permission_context(result.new_context.tool_permission_context);

            // Model-visibility by type: the tool_result user row becomes
            // a `Message` entry (CC's single array holds tool results), while
            // any other display row stays render-only.
            if let Some(tool_result) = result.message {
                append_history_rows(&mut history_state_for_permission, [tool_result]);
            }
        }
        let _ = mock_pump_tx_for_permission_cancel.try_send(());
    };

    let permission_context_for_sandbox = permission_store.clone();
    let app_store_for_sandbox = app_store.clone();
    let mut on_sandbox_permission_response = move |response: crate::components::permissions::sandbox_permission_request::SandboxPermissionResponse| {
        let current_request = {
            let app = app_store_for_sandbox.get();
            app.worker_sandbox_permissions.queue.first().cloned()
        };
        let Some(current_request) = current_request else {
            return;
        };
        let team_name = app_store_for_sandbox
            .get()
            .team_context
            .as_ref()
            .map(|context| context.team_name.clone())
            .or_else(|| {
                crate::hooks::use_inbox_poller::current_team_context_from_team_record()
                    .map(|context| context.team_name)
            });
        let _ = crate::utils::swarm::permission_sync::send_sandbox_permission_response_via_mailbox(
            &current_request.worker_name,
            &current_request.request_id,
            &current_request.host,
            response.allow,
            team_name.as_deref(),
        );
        if response.allow && response.persist_to_settings {
            let update = crate::types::permissions::PermissionUpdate::AddRules {
                destination: crate::types::permissions::PermissionUpdateDestination::LocalSettings,
                behavior: crate::types::permissions::PermissionBehavior::Allow,
                rules: vec![crate::types::permissions::PermissionRuleValue::new(
                    crate::tools::web_fetch_tool::prompt::WEB_FETCH_TOOL_NAME,
                    Some(format!("domain:{}", current_request.host)),
                )],
            };
            let current_context = permission_context_for_sandbox.tool_permission_context();
            let next_context = crate::utils::permissions::permission_update::apply_permission_update(
                &current_context,
                &update,
            );
            permission_context_for_sandbox.set_tool_permission_context(next_context.clone());
        }
        app_store_for_sandbox.replace_with(|app| {
            if !app.worker_sandbox_permissions.queue.is_empty() {
                // P4 identity: make_mut = CC's `{...prev.workerSandboxPermissions}`
                // spread before advancing the queue (selected_index clamp is a
                // pre-existing Rust extension, kept).
                let permissions = Arc::make_mut(&mut app.worker_sandbox_permissions);
                permissions.queue.remove(0);
                if permissions.selected_index >= permissions.queue.len()
                    && permissions.selected_index > 0
                {
                    permissions.selected_index -= 1;
                }
            }
        });
    };

    if should_exit.get() {
        // Maps to: CC REPL.tsx:4860-4886 handleExit -> commands/exit/exit.tsx:42
        // and ExitFlow.tsx:23-25 -> gracefulShutdown(0, 'prompt_input_exit').
        // Explicit normal exit overrides process.exitCode left by commands.
        // Other render-loop exits retain the existing Main status carrier.
        if let Some(code) = process_exit_code.as_ref() {
            code.store(0, std::sync::atomic::Ordering::SeqCst);
        }
        hooks.use_context_mut::<SystemContext>().exit();
    }

    // Maps to: CC `Messages.tsx` `useMemo(() => normalizeMessages(messages),
    // [messages])` — the render rows and the model view are per-commit
    // projections of the single history, cached on the history Arc identity so
    // the Messages memo key (`Arc::as_ptr`) stays stable across unrelated
    // renders. The cache holds the source Arc it projected, making
    // `Arc::ptr_eq` sound (a live allocation's address cannot be reused — a
    // raw-pointer `use_memo` key would be an ABA hazard after two consecutive
    // sets between renders).
    let history_snapshot = {
        let current = history_state.read();
        Arc::clone(&*current)
    };
    let mut history_projection_cache = hooks.use_ref(|| {
        Option::<(
            Arc<Vec<HistoryEntry>>,
            Arc<Vec<RenderableMessage>>,
            Arc<Vec<Message>>,
        )>::None
    });
    let (messages, model_messages) = {
        let cached = history_projection_cache.read().clone();
        match cached {
            Some((source, rows, model)) if Arc::ptr_eq(&source, &history_snapshot) => (rows, model),
            _ => {
                let rows = Arc::new(project_history_rows(&history_snapshot));
                let model = Arc::new(history_model_messages(&history_snapshot));
                history_projection_cache.set(Some((
                    Arc::clone(&history_snapshot),
                    Arc::clone(&rows),
                    Arc::clone(&model),
                )));
                (rows, model)
            }
        }
    };
    #[cfg(test)]
    if let Some(probe) = active_query_probe.as_ref() {
        *probe.snapshot.lock().unwrap() = Some(tests::ReplActiveQuerySnapshot {
            rows: messages.as_ref().clone(),
            model_messages: model_messages.as_ref().clone(),
            query_id: active_query.read().as_ref().map(|handle| handle.id.clone()),
            local_command_open: active_local_command_ui.read().is_some(),
        });
    }
    // Maps to: CC `REPL.tsx:5107`
    // `useLogMessages(messages, messages.length === initialMessages?.length)` —
    // the single transcript-recording site for the conversation. While the
    // history is still exactly the resumed prefix there is nothing new to
    // write.
    crate::hooks::use_log_messages::use_log_messages(
        &mut hooks,
        Arc::clone(&model_messages),
        props
            .initial_messages
            .as_ref()
            .is_some_and(|initial| initial.len() == model_messages.len()),
    );
    let streaming_text = streaming_text_preview
        .read()
        .as_ref()
        .and_then(|preview| visible_streaming_text(&preview.raw, STREAMING_TEXT_DISPLAY_MODE));
    let active_local_command_ui_snapshot = active_local_command_ui.read().clone();
    // Maps to CC processSlashCommand.tsx:733-813 and REPL.tsx:4336-4404.
    // Each invocation captures its own args/result route. Source immediate
    // callbacks clear the current overlay even after their child was replaced;
    // only the ordinary Promise route has first-resolution semantics.
    let plugin_invocation = active_local_command_ui_snapshot
        .clone()
        .filter(|active| matches!(active.panel, LocalCommandPanel::PluginSettings { .. }));
    let plugin_callback_key = plugin_invocation
        .as_ref()
        .map(|active| Arc::as_ptr(&active.completion) as usize);
    let plugin_completion_factory = {
        let notification_store = app_store.clone();
        move |plugin_invocation: Option<ActiveLocalCommandUi>| {
            let notification_store = notification_store.clone();
            Handler::from(move |output: Option<String>| {
                let Some(active) = plugin_invocation.as_ref() else {
                    return;
                };
                let Some(invocation) = active.invocation.as_ref() else {
                    return;
                };
                let LocalCommandPanel::PluginSettings {
                    preceding_input_blocks,
                    immediate_execution,
                    ..
                } = &active.panel
                else {
                    return;
                };
                // CC ordinary processSlashCommand resolves a Promise once. The
                // direct immediate callback has no such guard (REPL.tsx:4340).
                if !*immediate_execution && active.completion.set(()).is_err() {
                    return;
                }
                let mut history_state = history_state;
                let mut active_local_command_ui = active_local_command_ui;
                // CC REPL.tsx:4347-4351 + handlePromptSubmit.ts:547-552: no
                // invocation-identity check. Late completion clears a newer UI.
                active_local_command_ui.set(None);
                let command = &invocation.slash_command.command_name;
                let args = &invocation.slash_command.args;
                if *immediate_execution {
                    if let Some(output) = output.as_deref().filter(|text| !text.is_empty()) {
                        crate::context::notifications::NotificationsWriter::new(
                            notification_store.clone(),
                        )
                        .add_notification(
                            crate::context::notifications::Notification::text(
                                format!("immediate-{command}"),
                                output.to_string(),
                                crate::context::notifications::NotificationPriority::Immediate,
                            ),
                        );
                        if !crate::utils::fullscreen::is_fullscreen_env_enabled() {
                            append_history_rows(
                                &mut history_state,
                                local_command_result_messages(
                                    command,
                                    args,
                                    &crate::utils::xml::escape_xml(output),
                                    false,
                                ),
                            );
                        }
                    }
                } else {
                    // ValidatePlugin never requests shouldQuery, so the ordinary
                    // processUserInput hook tail returns these rows unchanged.
                    let result = crate::utils::process_user_input::process_slash_command::local_jsx_command_result(command, args, output.as_deref(), None, false, &[], preceding_input_blocks.clone());
                    append_history_rows(&mut history_state, result.messages);
                }
            })
        }
    };
    #[cfg(test)]
    if let Some(observer) = hooks.try_use_context::<tests::PluginCompletionObserver>() {
        *observer.factory.lock().unwrap() = Some(Arc::new(plugin_completion_factory.clone()));
        *observer.active.lock().unwrap() = Some(active_local_command_ui);
        *observer.history.lock().unwrap() = Some(history_state);
    }
    let plugin_on_complete: Handler<Option<String>> = hooks.use_memo(
        || plugin_completion_factory(plugin_invocation),
        plugin_callback_key,
    );
    let active_prompt_shell_command_snapshot = active_prompt_shell_command.read().clone();
    let exit_flow_active_snapshot = exit_flow_active.get();
    // Maps to: CC REPL.tsx:2689 `getFocusedInputDialog() === 'message-selector'`.
    let message_selector_visible_snapshot = message_selector_visible.get();
    let hint_recommendation_snapshot = hint_recommendation.read().clone();
    let active_startup_dialog = if show_remote_callout {
        Some(ReplStartupDialogKind::RemoteCallout)
    } else if hint_recommendation_snapshot.is_some() {
        Some(ReplStartupDialogKind::PluginHint)
    } else if show_desktop_upsell_startup.get() {
        Some(ReplStartupDialogKind::DesktopUpsell)
    } else {
        None
    };
    let current_permission = permission_queue.read().first().cloned();
    let query_is_loading = !pending_responses.read().is_empty() || active_query.read().is_some();
    let user_input_on_processing_active = user_input_on_processing
        .read()
        .as_ref()
        .is_some_and(|input| !input.is_empty());
    // CC's QueryGuard reservation makes input processing loading-like before
    // the async query is installed. Preserve that for Messages/static output,
    // PromptInput, and the idle-leader spinner projection.
    let is_loading = query_is_loading || user_input_on_processing_active;
    let spinner_app_snapshot = app_store.get();
    let has_running_teammates =
        !crate::components::spinner::teammate_tree::running_teammate_spinner_tasks_from_app_tasks(
            &spinner_app_snapshot.tasks,
        )
        .is_empty();
    let pending_worker_request = spinner_app_snapshot.pending_worker_request.is_some();
    let is_brief_only = spinner_app_snapshot.is_brief_only;
    let in_progress_tool_use_ids_value = in_progress_tool_use_ids.read().clone();
    let streaming_tool_use_ids_value = streaming_tool_use_ids.read().clone();
    // CC threads `conversationId` into Messages as a prop (Messages.tsx:262);
    // the row key is `${msg.uuid}-${conversationId}` (Messages.tsx:792-795).
    let conversation_generation = conversation_id.get();
    let only_sleep_tool_active =
        only_sleep_tool_active(&model_messages, &in_progress_tool_use_ids_value);
    // Maps to: CC `screens/REPL.tsx:2245-2263` `showSpinner`, including the
    // input-processing, teammate, queue, worker, Sleep-only, and brief-only
    // branches. Local command/shell panels are the Rust toolJSX projection;
    // none currently opt into `showSpinner: true`.
    let show_spinner = should_show_spinner(ShowSpinnerInput {
        tool_jsx_allows_spinner: active_local_command_ui_snapshot.is_none()
            && active_prompt_shell_command_snapshot.is_none(),
        tool_use_confirm_queue_empty: current_permission.is_none(),
        // Prompt requests that need input share the focused permission/dialog
        // branches in this port and return before the main-screen spinner.
        prompt_queue_empty: true,
        is_loading: query_is_loading,
        user_input_on_processing: user_input_on_processing_active,
        has_running_teammates,
        command_queue_len: queued_commands.len(),
        pending_worker_request,
        only_sleep_tool_active,
        visible_streaming_text: streaming_text.is_some(),
        is_brief_only,
    });
    // CC brief-only mode suppresses streamed assistant text; the spinner is
    // retained as its feedback instead.
    let streaming_text_for_messages = if is_brief_only {
        None
    } else {
        streaming_text.clone()
    };
    let stop_hook_spinner_suffix_value =
        stop_hook_spinner_suffix(&stop_hook_spinner_state.read(), is_loading);
    let is_empty_session = messages.is_empty() && !is_loading;
    let main_screen_width = terminal_cols.max(1) as u32;
    let resume_store_snapshot = resume_restore_stores.read().clone();
    // AppState.teamContext / agent are the single source (CC).
    let inbox_team_context =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.team_context.clone());
    let app_agent = crate::state::app_state::use_app_state(&mut hooks, |state| state.agent.clone());
    let is_in_process_teammate = inbox_poller_state.read().is_in_process_teammate;
    let teammate_team_name = crate::utils::teammate::get_team_name(
        inbox_team_context
            .as_ref()
            .map(|context| context.team_name.as_str()),
    );
    let swarm_banner_input = crate::components::prompt_input::use_swarm_banner::SwarmBannerInput {
        is_external_teammate: crate::utils::teammate::is_teammate() && !is_in_process_teammate,
        teammate_name: crate::utils::teammate::get_agent_name(),
        teammate_team_name,
        teammate_color: inbox_team_context
            .as_ref()
            .and_then(|context| context.self_agent_color.clone())
            .or_else(crate::utils::teammate::get_teammate_color),
        has_teammates: inbox_team_context
            .as_ref()
            .is_some_and(|context| !context.teammates.is_empty()),
        inside_tmux: Some(crate::utils::swarm::backends::detection::is_inside_tmux_sync()),
        in_process_mode: crate::utils::swarm::backends::registry::is_in_process_enabled(),
        native_panes: inbox_team_context.as_ref().is_some_and(|context| {
            context.teammates.values().any(|teammate| {
                teammate
                    .backend_type
                    .as_deref()
                    .is_some_and(|backend| backend.eq_ignore_ascii_case("iterm2"))
            })
        }),
        standalone_name: crate::state::app_state::use_app_state(&mut hooks, |state| {
            state
                .standalone_agent_context
                .as_ref()
                .map(|context| context.name.clone())
        }),
        standalone_color: crate::state::app_state::use_app_state(&mut hooks, |state| {
            state
                .standalone_agent_context
                .as_ref()
                .and_then(|context| context.color.clone())
        }),
        cli_agent: app_agent.or_else(|| resume_store_snapshot.agent_setting.clone()),
        swarm_socket_name: Some(crate::utils::swarm::constants::get_swarm_socket_name()),
        ..Default::default()
    };

    // Maps to: CC `REPL.tsx:1197-1221` — `useMergedTools(combinedInitialTools,
    // mcp.tools, toolPermissionContext)` narrowed by the `resolveAgentTools`
    // memo. Its `tools` half is what both `<Messages>` sites receive
    // (`:5821`, `:6162`) and what `AgentTool/UI.tsx:1096`'s `findToolByName`
    // searches. Held behind `use_memo` because the assembly is not free —
    // `agent_tool_schema()` reloads the agent definitions off disk — where CC's
    // `useMemo` guards only re-derivation.
    //
    // Both store reads take the `Arc`s off ONE `AppState` snapshot rather than
    // `tool_permission_context()` / `McpWriter::current()`, whose deep clones
    // would land on every frame ahead of the memo that exists to avoid exactly
    // that. `McpWriter::current` is `(*store.get().mcp).clone()`
    // (`app_state_store.rs:334-336`), so this is the same value.
    //
    // Pre-existing seam, not introduced here: CC subscribes to both inputs
    // whole (`REPL.tsx:972` `useAppState(s => s.toolPermissionContext)`,
    // `:974` `useAppState(s => s.mcp)`) while this body subscribes only to
    // `state.mcp.commands` (`:2612`, recorded there). These are snapshot reads
    // on top of that narrower subscription, so a pool change lands on the next
    // render rather than waking one of its own.
    let app_state_for_tool_pool = permission_store.get();
    let render_tool_pool_permission_context =
        Arc::clone(&app_state_for_tool_pool.tool_permission_context);
    let render_tool_pool_mcp_state = Arc::clone(&app_state_for_tool_pool.mcp);
    let render_tool_pool_agent_definition = main_thread_agent_definition.read().clone();
    let tools = hooks.use_memo(
        || {
            repl_render_tool_pool(
                &render_tool_pool_permission_context,
                &render_tool_pool_mcp_state,
                &initial_tools,
                render_tool_pool_agent_definition.as_deref(),
            )
        },
        repl_render_tool_pool_deps(
            &render_tool_pool_permission_context,
            &render_tool_pool_mcp_state,
            &initial_tools,
            render_tool_pool_agent_definition.as_deref(),
        ),
    );

    // Maps to: CC `getFocusedInputDialog()` head (REPL.tsx:2686-2694): exit
    // states always take precedence, then the message selector, and interrupt
    // dialogs are suppressed while the user is actively typing
    // (`isPromptInputActive`); only then does
    // `sandboxPermissionRequestQueue[0]` focus, outranking tool-permission.
    // (CC `isExiting` corresponds to the immediate `should_exit` exit above;
    // `exitFlow` to `exit_flow_active`.)
    let current_local_sandbox_ask = if exit_flow_active_snapshot
        || message_selector_visible_snapshot
        || is_prompt_input_active.get()
    {
        None
    } else {
        sandbox_permission_request_queue.read().first().cloned()
    };
    if let Some(current_ask) = current_local_sandbox_ask {
        let permission_store_for_local_sandbox = permission_store.clone();
        let mut sandbox_queue_for_response = sandbox_permission_request_queue;
        // Maps to: CC REPL.tsx:6292-6355 sandbox dialog `onUserResponse`.
        let on_local_sandbox_response = move |response: crate::components::permissions::sandbox_permission_request::SandboxPermissionResponse| {
            let mut queue = sandbox_queue_for_response.read().clone();
            handle_local_sandbox_permission_response(
                &mut queue,
                response,
                &mut |approved_host, allow| {
                    // Maps to: CC REPL.tsx:6302-6330 `persistToSettings` branch.
                    let update = crate::types::permissions::PermissionUpdate::AddRules {
                        destination: crate::types::permissions::PermissionUpdateDestination::LocalSettings,
                        behavior: if allow {
                            crate::types::permissions::PermissionBehavior::Allow
                        } else {
                            crate::types::permissions::PermissionBehavior::Deny
                        },
                        rules: vec![crate::types::permissions::PermissionRuleValue::new(
                            crate::tools::web_fetch_tool::prompt::WEB_FETCH_TOOL_NAME,
                            Some(format!("domain:{approved_host}")),
                        )],
                    };
                    // CC REPL.tsx:6317-6323: apply to the live permission
                    // context first (setAppState + applyPermissionUpdate).
                    let next = crate::utils::permissions::permission_update::apply_permission_updates(
                        &permission_store_for_local_sandbox.tool_permission_context(),
                        std::slice::from_ref(&update),
                    );
                    permission_store_for_local_sandbox.set_tool_permission_context(next);
                    // CC REPL.tsx:6325 `persistPermissionUpdate(update)` —
                    // fire-and-forget: CC logs and swallows persistence
                    // failures (addPermissionRulesToSettings catch), and the
                    // in-memory rule applied above still governs the session.
                    if let Err(error) =
                        crate::utils::permissions::permission_update::persist_permission_updates(
                            std::slice::from_ref(&update),
                        )
                    {
                        crate::utils::debug::log_for_debugging(&format!(
                            "Could not persist sandbox network permission: {error:#}"
                        ));
                    }
                    // CC REPL.tsx:6327-6329 `SandboxManager.refreshConfig()`:
                    // update the in-memory network policy immediately so
                    // pending requests can't slip through before the settings
                    // change is detected.
                    let settings = crate::utils::settings::get_initial_settings();
                    let runtime_config = crate::utils::sandbox::sandbox_adapter::convert_to_sandbox_runtime_config(&settings);
                    if let Err(error) = crate::utils::sandbox::network_proxy::ensure_network_proxy(&runtime_config.network) {
                        crate::utils::debug::log_for_debugging(&format!(
                            "Could not refresh sandbox network policy: {error:#}"
                        ));
                    }
                },
            );
            sandbox_queue_for_response.set(queue);
        };
        return element! {
            View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                ClearTerminalOnResize(cols: terminal_cols, rows: terminal_rows)
                ClearScreenOnGeneration(generation: redraw_generation.get())
                #(if !messages.is_empty() {
                    Some(memoized_messages(
                        messages.clone(),
                        conversation_generation,
                        false,
                        verbose,
                        false,
                        terminal_cols,
                        terminal_rows,
                        None,
                        classifier_approvals.read().clone(),
                        status_notice_context.clone(),
                        None,
                        Arc::clone(&in_progress_tool_use_ids_value),
                        Arc::clone(&streaming_tool_use_ids_value),
                        Arc::clone(&tools),
                    ))
                } else {
                    None
                })
                SandboxPermissionRequest(
                    // Maps to: CC REPL.tsx:6290
                    // `key={sandboxPermissionRequestQueue[0]!.hostPattern.host}`
                    // — remount (reset selection) when the host changes.
                    key: current_ask.host_pattern.host.clone(),
                    host_pattern: Some(current_ask.host_pattern.clone()),
                    on_user_response: on_local_sandbox_response,
                )
            }
        }
        .into_any();
    }

    if let Some(confirm) = current_permission.clone() {
        let worker_badge = confirm.worker_badge.map(permission_worker_badge_to_ui);
        let request = confirm.request;
        let permission_context_snapshot = permission_store.tool_permission_context();
        let pending_permission_tool_use_id = request.tool_use_id.clone();
        // Maps to: CC `screens/REPL.tsx:6034-6035` permission overlay remount key.
        let permission_request_key = request.tool_use_id.clone();
        // Main-screen/native-scrollback path only. Keep the official
        // sequential order explicitly in REPL: {scrollable}{overlay}.
        // PromptInput is hidden while the permission dialog is focused,
        // matching CC's focusedInputDialog behavior.
        return element! {
            View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                ClearTerminalOnResize(cols: terminal_cols, rows: terminal_rows)
                ClearScreenOnGeneration(generation: redraw_generation.get())
                #(if !messages.is_empty() {
                    Some(memoized_messages(
                        messages.clone(),
                        conversation_generation,
                        false,
                        verbose,
                        false,
                        terminal_cols,
                        terminal_rows,
                        Some(pending_permission_tool_use_id.clone()),
                        classifier_approvals.read().clone(),
                        status_notice_context.clone(),
                        None,
                        Arc::clone(&in_progress_tool_use_ids_value),
                        Arc::clone(&streaming_tool_use_ids_value),
                        Arc::clone(&tools),
                    ))
                } else {
                    None
                })
                PermissionRequest(
                    key: permission_request_key,
                    request: Some(request),
                    tool_permission_context: Some(permission_context_snapshot),
                    worker_badge: worker_badge,
                    // Maps to: CC `REPL.tsx:6045` `verbose={verbose}`, read from
                    // `useAppState(s => s.verbose)` at `:973` — the producer the
                    // port's `PermissionRequestProps.verbose` seam was waiting on.
                    verbose: verbose,
                    messages: Arc::clone(&model_messages),
                    on_select_response: on_permission_response,
                    on_cancel: on_permission_cancel,
                )
            }
        }
        .into_any();
    }

    let current_sandbox_permission = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.worker_sandbox_permissions.queue.first().cloned()
    });
    if let Some(sandbox_request) = current_sandbox_permission {
        let pending_permission_tool_use_id = format!("sandbox:{}", sandbox_request.request_id);
        return element! {
                View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                    ClearTerminalOnResize(cols: terminal_cols, rows: terminal_rows)
                    ClearScreenOnGeneration(generation: redraw_generation.get())
                    #(if !messages.is_empty() {
                        Some(memoized_messages(
                            messages.clone(),
                            conversation_generation,
                            false,
                            verbose,
                            false,
                            terminal_cols,
                            terminal_rows,
                            Some(pending_permission_tool_use_id.clone()),
                            classifier_approvals.read().clone(),
                            status_notice_context.clone(),
                            None,
                            Arc::clone(&in_progress_tool_use_ids_value),
                            Arc::clone(&streaming_tool_use_ids_value),
                            Arc::clone(&tools),
                        ))
                    } else {
                        None
                    })
                    SandboxPermissionRequest(
                        host_pattern: Some(crate::utils::sandbox::sandbox_adapter::NetworkHostPattern::new(sandbox_request.host)),
                        on_user_response: on_sandbox_permission_response,
                    )
                }
            }
        .into_any();
    }

    // Maps to: CC REPL `{pendingWorkerRequest && <WorkerPendingPermission .../>}`.
    let pending_worker_request = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.pending_worker_request.clone()
    });
    if let Some(pending) = pending_worker_request {
        let pending_permission_tool_use_id = pending.tool_use_id.clone();
        return element! {
            View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                ClearTerminalOnResize(cols: terminal_cols, rows: terminal_rows)
                ClearScreenOnGeneration(generation: redraw_generation.get())
                #(if !messages.is_empty() {
                    Some(memoized_messages(
                        messages.clone(),
                        conversation_generation,
                        false,
                        verbose,
                        false,
                        terminal_cols,
                        terminal_rows,
                        Some(pending_permission_tool_use_id.clone()),
                        classifier_approvals.read().clone(),
                        status_notice_context.clone(),
                        None,
                        Arc::clone(&in_progress_tool_use_ids_value),
                        Arc::clone(&streaming_tool_use_ids_value),
                        Arc::clone(&tools),
                    ))
                } else {
                    None
                })
                WorkerPendingPermission(
                    tool_name: pending.tool_name.clone(),
                    description: pending.description.clone(),
                )
            }
        }
        .into_any();
    }

    // Maps to: CC REPL `{pendingSandboxRequest && <WorkerPendingPermission toolName="Network Access" .../>}`.
    let pending_sandbox_request = crate::state::app_state::use_app_state(&mut hooks, |state| {
        state.pending_sandbox_request.clone()
    });
    if let Some(pending) = pending_sandbox_request {
        let pending_permission_tool_use_id = format!("sandbox:{}", pending.request_id);
        let description = format!(
            "Waiting for leader to approve network access to {}",
            pending.host
        );
        return element! {
            View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                ClearTerminalOnResize(cols: terminal_cols, rows: terminal_rows)
                ClearScreenOnGeneration(generation: redraw_generation.get())
                #(if !messages.is_empty() {
                    Some(memoized_messages(
                        messages.clone(),
                        conversation_generation,
                        false,
                        verbose,
                        false,
                        terminal_cols,
                        terminal_rows,
                        Some(pending_permission_tool_use_id.clone()),
                        classifier_approvals.read().clone(),
                        status_notice_context.clone(),
                        None,
                        Arc::clone(&in_progress_tool_use_ids_value),
                        Arc::clone(&streaming_tool_use_ids_value),
                        Arc::clone(&tools),
                    ))
                } else {
                    None
                })
                WorkerPendingPermission(
                    tool_name: "Network Access".to_string(),
                    description: description,
                )
            }
        }
        .into_any();
    }

    let current_elicitation = app_store.get().elicitation.queue.first().cloned();
    if let Some(elicitation_event) = current_elicitation {
        let response_server_name = elicitation_event.server_name.clone();
        let response_request_id = elicitation_event.request_id.clone();
        let waiting_server_name = elicitation_event.server_name.clone();
        let waiting_request_id = elicitation_event.request_id.clone();
        let is_error_retry_elicitation = elicitation_event
            .waiting_state
            .as_ref()
            .is_some_and(|state| state.show_cancel);
        let elicitation_store_for_response = app_store.clone();
        let elicitation_store_for_dismiss = app_store.clone();
        let on_elicitation_response =
            move |response: crate::services::mcp::elicitation_handler::ElicitationResult| {
                let keep_url_accept = response.action == ElicitationAction::Accept
                    && matches!(
                        elicitation_store_for_response
                            .get()
                            .elicitation
                            .queue
                            .first()
                            .map(|event| &event.params),
                        Some(ElicitationRequestParams::Url { .. })
                    );

                if !(keep_url_accept && is_error_retry_elicitation) {
                    let server_name = response_server_name.clone();
                    let request_id = response_request_id.clone();
                    let response_to_send = response.clone();
                    let send_response = async move {
                        respond_to_mcp_elicitation(&server_name, &request_id, response_to_send)
                            .await;
                    };
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn(send_response);
                    } else {
                        futures::executor::block_on(send_response);
                    }
                }

                if !keep_url_accept {
                    elicitation_store_for_response.replace_with(|app| {
                        let mut state = (*app.elicitation).clone();
                        state.pop_front();
                        app.elicitation = std::sync::Arc::new(state);
                    });
                }
            };
        let on_elicitation_waiting_dismiss = move |action: ElicitationWaitingDismissAction| {
            if is_error_retry_elicitation {
                let server_name = waiting_server_name.clone();
                let request_id = waiting_request_id.clone();
                let response_to_send = url_retry_waiting_dismiss_result(action);
                let send_response = async move {
                    respond_to_mcp_elicitation(&server_name, &request_id, response_to_send).await;
                };
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(send_response);
                } else {
                    futures::executor::block_on(send_response);
                }
            }
            elicitation_store_for_dismiss.replace_with(|app| {
                let mut state = (*app.elicitation).clone();
                state.pop_front();
                app.elicitation = std::sync::Arc::new(state);
            });
        };
        return element! {
            View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                ClearTerminalOnResize(cols: terminal_cols, rows: terminal_rows)
                ClearScreenOnGeneration(generation: redraw_generation.get())
                #(if !messages.is_empty() {
                    Some(memoized_messages(
                        messages.clone(),
                        conversation_generation,
                        false,
                        verbose,
                        false,
                        terminal_cols,
                        terminal_rows,
                        None,
                        classifier_approvals.read().clone(),
                        status_notice_context.clone(),
                        None,
                        Arc::clone(&in_progress_tool_use_ids_value),
                        Arc::clone(&streaming_tool_use_ids_value),
                        Arc::clone(&tools),
                    ))
                } else {
                    None
                })
                ElicitationDialog(
                    event: Some(elicitation_event),
                    on_response: on_elicitation_response,
                    on_waiting_dismiss: on_elicitation_waiting_dismiss,
                )
            }
        }
        .into_any();
    }

    let active_is_resume = active_local_command_ui_snapshot
        .as_ref()
        .is_some_and(ActiveLocalCommandUi::is_resume);
    let should_render_prompt_input = screen.get() == Screen::Prompt
        && !exit_flow_active_snapshot
        && active_startup_dialog.is_none()
        // CC: PromptInput unmounts while the message selector is active.
        && !message_selector_visible_snapshot
        && active_local_command_ui_snapshot
            .as_ref()
            .is_none_or(|active| !active.should_hide_prompt_input);
    let is_local_command_ui_active_for_prompt = active_local_command_ui_snapshot
        .as_ref()
        .is_some_and(|active| active.is_local_command_ui);
    // Maps to official `filterResumableSessions(logs, currentSessionId)`: after
    // resume adoption, keep the current session out of the next picker open.
    let current_resume_session_id = resume_store_snapshot.session_id.clone();
    let current_resume_session_name = resume_store_snapshot.custom_title.clone();
    // Maps to `commands/hooks/hooks.tsx`: pass the enabled built-in tool names;
    // HooksConfigMenu adds live MCP tool names from AppState.
    let agents_tools = if active_local_command_ui_snapshot
        .as_ref()
        .is_some_and(|active| matches!(active.panel, LocalCommandPanel::Agents))
    {
        crate::tools::get_tools(&permission_store.tool_permission_context())
            .into_iter()
            .map(|tool| crate::components::agents::tool_selector::AgentToolOption::new(tool.name))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let hooks_tool_names = if active_local_command_ui_snapshot
        .as_ref()
        .is_some_and(|active| matches!(active.panel, LocalCommandPanel::Hooks))
    {
        crate::tools::get_tools(&permission_store.tool_permission_context())
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let on_exit_flow_done = move |done: ExitFlowDone| {
        if let Some(message) = done.message {
            match done.display {
                Some(crate::utils::worktree::CommandResultDisplay::Skip) => {}
                Some(crate::utils::worktree::CommandResultDisplay::User) => {
                    // display:'user' → a user message in CC's single array.
                    let mut history_state = history_state;
                    append_history_rows(
                        &mut history_state,
                        [RenderableMessage::user(
                            Uuid::new_v4().to_string(),
                            format!("<local-command-stdout>{message}</local-command-stdout>"),
                        )],
                    );
                }
                Some(crate::utils::worktree::CommandResultDisplay::System) => {
                    let mut history_state = history_state;
                    push_system_notice(&mut history_state, SystemMessageLevel::Info, message);
                }
                None => {
                    // Maps to: CC /exit — `onDone(getRandomGoodbyeMessage())`
                    // followed immediately by gracefulShutdown: the goodbye
                    // is persisted as the /exit command's output in the
                    // session JSONL and is only visible on resume replay
                    // (`> /exit` + `⎿ Goodbye!`). Nothing is rendered into
                    // the live message stream. Wire `message` into the
                    // /exit command-output persistence when that dedicated
                    // command-output recorder is ported.
                    let _goodbye_for_session_write = message;
                }
            }
        }
        exit_flow_active.set(false);
        should_exit.set(true);
    };
    let on_exit_flow_cancel = move |_: ()| {
        exit_flow_active.set(false);
    };
    let app_store_for_remote_done = app_store.clone();
    let on_remote_callout_done = move |done: RemoteCalloutDone| {
        // Official REPL enables bridge app-state when selection is `enable`.
        // Safe Cometix only dismisses this startup dialog; bridge runtime work
        // belongs to the dedicated Remote Control/bridge slice.
        let _selection = done.selection;
        let _would_mark_seen = done.would_mark_seen;
        // B3 flip-audit: CC REPL.tsx:6610-6614 — `if (!prev.showRemoteCallout)
        // return prev` before the dismissing spread.
        app_store_for_remote_done.set_state(|prev| {
            if !prev.show_remote_callout {
                return crate::state::store::UpdateDecision::Same(());
            }
            let mut next = (**prev).clone();
            next.show_remote_callout = false;
            crate::state::store::UpdateDecision::Replace {
                next: Arc::new(next),
                result: (),
            }
        });
    };
    let mut plugin_hint_notifications =
        crate::context::notifications::NotificationsWriter::new(app_store.clone());
    let app_store_for_plugin_install = app_store.clone();
    let mut hint_recommendation_for_response = hint_recommendation;
    let on_plugin_hint_response =
        move |response: crate::components::claude_code_hint::PluginHintResponse| {
            let Some(recommendation) = hint_recommendation_for_response.read().clone() else {
                return;
            };
            let record_error = crate::utils::plugins::hint_recommendation::mark_hint_plugin_shown(
                &recommendation.plugin_id,
            )
            .err()
            .map(|error| error.to_string());
            let disable_error = (response
                == crate::components::claude_code_hint::PluginHintResponse::Disable)
                .then(crate::utils::plugins::hint_recommendation::disable_hint_recommendations)
                .and_then(Result::err)
                .map(|error| error.to_string());
            if response == crate::components::claude_code_hint::PluginHintResponse::Yes {
                let spawn_result =
                    crate::hooks::use_plugin_recommendation_base::install_plugin_and_notify(
                        recommendation.plugin_id.clone(),
                        recommendation.plugin_name.clone(),
                        "hint-plugin",
                        app_store_for_plugin_install.clone(),
                        {
                            // CC useClaudeCodeHintRecommendation.tsx:96-108:
                            // the common notification helper receives its install callback.
                            let plugin_id = recommendation.plugin_id.clone();
                            let marketplace_name = recommendation.marketplace_name.clone();
                            move |plugin_data| async move {
                                use crate::utils::plugins::plugin_installation_helpers::{
                                    InstallPluginParams, InstallPluginResult,
                                    install_plugin_from_marketplace,
                                };
                                match install_plugin_from_marketplace(InstallPluginParams {
                                    plugin_id,
                                    entry: plugin_data.entry,
                                    marketplace_name,
                                    scope: Some(crate::utils::plugins::schemas::PluginScope::User),
                                    trigger: Some("hint".into()),
                                })
                                .await
                                {
                                    InstallPluginResult::Success { .. } => Ok(()),
                                    InstallPluginResult::Failure { error } => {
                                        Err(anyhow::anyhow!(error))
                                    }
                                }
                            }
                        },
                    );
                if let Err(error) = spawn_result {
                    crate::utils::debug::log_for_debugging(&format!(
                        "Plugin hint installer thread could not start: {error}"
                    ));
                    plugin_hint_notifications.add_notification(
                        crate::context::notifications::Notification::text(
                            "hint-plugin-install-failed",
                            format!("Failed to install {}", recommendation.plugin_name),
                            crate::context::notifications::NotificationPriority::Immediate,
                        )
                        .with_color(crate::context::notifications::NotificationColor::Error)
                        .with_timeout_ms(5_000),
                    );
                }
            }
            if let Some(error) = record_error.as_ref().or(disable_error.as_ref()) {
                plugin_hint_notifications.add_notification(
                    crate::context::notifications::Notification::text(
                        format!("plugin-hint-state: {}", recommendation.plugin_id),
                        format!("Could not persist plugin hint preference: {error}"),
                        crate::context::notifications::NotificationPriority::High,
                    )
                    .with_color(crate::context::notifications::NotificationColor::Error),
                );
            }
            hint_recommendation_for_response.set(None);
        };
    let on_desktop_upsell_done = move |done: DesktopUpsellDone| {
        // Official startup upsell ignores DesktopHandoff result payloads and
        // just dismisses. Safe Cometix also avoids config writes and handoff
        // side effects; the callback keeps would-* metadata internal.
        let _done = done;
        show_desktop_upsell_startup.set(false);
    };
    let transcript_footer_text = if screen.get() == Screen::Transcript {
        let runtime_bindings = keybinding_runtime_for_transcript_display
            .as_ref()
            .map(|runtime| runtime.bindings());
        let toggle = runtime_bindings.as_ref().map_or_else(
            || "ctrl+o".to_string(),
            |bindings| {
                crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
                    "app:toggleTranscript",
                    &crate::keybindings::types::ContextName::Global,
                    "ctrl+o",
                    bindings,
                )
            },
        );
        let show_all = runtime_bindings.as_ref().map_or_else(
            || "ctrl+e".to_string(),
            |bindings| {
                crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
                    "transcript:toggleShowAll",
                    &crate::keybindings::types::ContextName::Transcript,
                    "ctrl+e",
                    bindings,
                )
            },
        );
        Some(format!(
            "Showing detailed transcript · {toggle} to toggle · {show_all} to {}",
            if show_all_in_transcript.get() {
                "collapse"
            } else {
                "show all"
            }
        ))
    } else {
        None
    };

    element! {
        // Maps to: CC `REPL.tsx:6134-6138` — `<MCPConnectionManager
        // dynamicMcpConfig isStrictMcpConfig>` WRAPS the rest of the tree. It
        // owns the connection effect and publishes the context that
        // `useMcpReconnect`/`useMcpToggleEnabled` read, so every MCP menu below
        // is inside its provider.
        crate::services::mcp::mcp_connection_manager::McpConnectionManager(
            app_store: mcp_manager_store.clone(),
            mcp_startup: props.mcp_startup.clone(),
        ) {
            View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                // CC mounts AnimatedTerminalTitle in both the transcript and
                // main returns (:5852, :6084) because they are separate JSX
                // trees; this single tree parents both screens, so one mount
                // is the same coverage. isAnimating mirrors :1578-1589:
                // loading, not waiting for approval (confirm queue, worker,
                // sandbox; the prompt queue shares the focused dialog branch
                // here), and no local-JSX command showing. noPrefix maps to
                // showStatusInTerminalTab — the tab-status feature is an
                // unported seam, so it stays false.
                AnimatedTerminalTitle(
                    is_animating: is_loading
                        && current_permission.is_none()
                        && spinner_app_snapshot.pending_worker_request.is_none()
                        && spinner_app_snapshot.pending_sandbox_request.is_none()
                        && active_local_command_ui_snapshot.is_none(),
                    title: terminal_tab_title.clone(),
                    disabled: terminal_title_disabled,
                    no_prefix: false,
                )
                ClearTerminalOnResize(cols: terminal_cols, rows: terminal_rows)
                ClearScreenOnGeneration(generation: redraw_generation.get())

            // Empty-session LogoHeader-equivalent. Official Messages.tsx owns
            // both LogoV2 and StatusNotices even before the first rendered row.
            // Keep the Messages owner mounted while the resume UI is active;
            // hiding LogoHeader is a prop change, not an ownership transfer.
            #(if is_empty_session {
                Some(memoized_messages_for_screen(
                    messages.clone(),
                    conversation_generation,
                    false,
                    verbose,
                    active_is_resume,
                    terminal_cols,
                    terminal_rows,
                    None,
                    classifier_approvals.read().clone(),
                    status_notice_context.clone(),
                    None,
                    screen.get(),
                    show_all_in_transcript.get(),
                    Arc::clone(&in_progress_tool_use_ids_value),
                    Arc::clone(&streaming_tool_use_ids_value),
                    Arc::clone(&tools),
                ))
            } else {
                None
            })

            // Messages remains the sole owner of the complete retained
            // projection and its row memo/OffscreenFreeze boundaries.
            #(if !messages.is_empty() || is_loading {
                Some(memoized_messages_for_screen(
                    messages.clone(),
                    conversation_generation,
                    is_loading,
                    verbose,
                    active_is_resume,
                    terminal_cols,
                    terminal_rows,
                    None,
                    classifier_approvals.read().clone(),
                    status_notice_context.clone(),
                    streaming_text_for_messages.clone(),
                    screen.get(),
                    show_all_in_transcript.get(),
                    Arc::clone(&in_progress_tool_use_ids_value),
                    Arc::clone(&streaming_tool_use_ids_value),
                    Arc::clone(&tools),
                ))
            } else {
                None
            })

            // Non-immediate local command UI maps to official scrollable/document
            // flow. It renders after Messages and before the bottom slot.
            #(active_local_command_ui_snapshot.clone().and_then(|active| {
                if active.is_immediate {
                    return None;
                }
                let clipboard_result = on_clipboard_command_result(active.clone());
                match active.panel {
                    LocalCommandPanel::PluginSettings { data, .. } => Some(element! {
                        crate::commands::plugin::plugin_settings::PluginSettings(args: data.args, on_complete: plugin_on_complete.clone())
                    }.into_any()),
                    LocalCommandPanel::Settings(tab) => Some(element! {
                        Settings(
                            on_close: on_local_command_ui_close,
                            on_result: on_local_command_ui_result,
                            default_tab: Some(tab),
                            session_id: current_resume_session_id.clone(),
                            session_name: current_resume_session_name.clone(),
                        )
                    }.into_any()),
                    LocalCommandPanel::Resume => Some(element! {
                        View(flex_direction: FlexDirection::Column) {
                            ClearLiveOutputOnMount
                            ResumeCommand(
                                command_stdout: Some(command_stdout.clone()),
                                current_session_id: current_resume_session_id.clone(),
                                on_close: { let done = clipboard_result.clone(); move |()| done("Resume cancelled".to_string()) },
                                on_select: {
                                    let on_resume_select = on_resume_select.clone();
                                    let active = active.clone();
                                    move |selection| on_resume_select(active.clone(), selection)
                                },
                                on_result: clipboard_result.clone(),
                            )
                        }
                    }.into_any()),
                    LocalCommandPanel::Help => Some(element! {
                        HelpV2(on_close: on_local_command_ui_close, commands: Some(commands.clone()))
                    }.into_any()),
                    LocalCommandPanel::Model => Some(element! {
                        model::ModelPickerWrapper(
                            on_close: on_local_command_ui_close,
                            on_select: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Effort {
                        args,
                        has_conversation_messages,
                    } => Some(element! {
                        EffortPicker(
                            args: args.clone(),
                            has_conversation_messages: has_conversation_messages,
                            on_close: on_local_command_ui_close,
                            on_cancel_args: on_local_command_ui_result.clone(),
                            on_select: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Fast => {
                        let initial_enabled = app_store.get().fast_mode;
                        let mut on_result = on_local_command_ui_result.clone();
                        let mut on_close = on_local_command_ui_close.clone();
                        let store_for_fast = app_store.clone();
                        // Maps to: CC fast.tsx:252-258 — read the real
                        // unavailable reason before mounting the picker. CC's
                        // `await prefetchFastModeStatus()` (:238) resolves the
                        // org status first; Rust reads the synchronously
                        // cached org status, same as the PromptInput mount.
                        // P4 review fix (2026-08-02).
                        let fast_unavailable_reason =
                            crate::utils::fast_mode::get_fast_mode_unavailable_reason();
                        Some(element! {
                            fast::FastModePicker(
                                initial_enabled: initial_enabled,
                                unavailable_reason: fast_unavailable_reason,
                                on_done: move |choice: Option<bool>| {
                                    match choice {
                                        Some(enabled) => {
                                            // Maps to: CC fast.tsx:35-58 applyFastMode —
                                            // clearFastModeCooldown (:39), persist to
                                            // userSettings (:40-42, `fastMode: enable ?
                                            // true : undefined`; Null carries the
                                            // undefined delete), and the ON-branch model
                                            // switch when the current model lacks
                                            // fast-mode support (:44-53). Realized
                                            // 2026-08-02 (P4 batch).
                                            crate::utils::fast_mode::clear_fast_mode_cooldown();
                                            let _ = crate::utils::settings::update_settings_for_source(
                                                crate::utils::settings::SettingSource::User,
                                                &serde_json::Map::from_iter([(
                                                    "fastMode".to_string(),
                                                    if enabled {
                                                        serde_json::Value::Bool(true)
                                                    } else {
                                                        serde_json::Value::Null
                                                    },
                                                )]),
                                            );
                                            store_for_fast.replace_with(|state| {
                                                if enabled
                                                    && !crate::utils::fast_mode::is_fast_mode_supported_by_model(
                                                        state.main_loop_model.as_deref(),
                                                    )
                                                {
                                                    state.main_loop_model =
                                                        Some(crate::utils::fast_mode::get_fast_mode_model());
                                                    state.main_loop_model_for_session = None;
                                                }
                                                state.fast_mode = enabled;
                                            });
                                            let msg = if enabled {
                                                "Fast mode ON".to_string()
                                            } else {
                                                "Fast mode OFF".to_string()
                                            };
                                            on_result(msg);
                                        }
                                        None => on_close(()),
                                    }
                                },
                            )
                        }.into_any())
                    },
                    LocalCommandPanel::Theme => Some(element! {
                        theme::ThemePickerWrapper(
                            on_close: on_local_command_ui_close,
                            on_select: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Export(data, _) => {
                        let on_result = clipboard_result.clone();
                        Some(element! {
                            ExportDialog(
                                command_stdout: Some(command_stdout.clone()),
                                content: data.content.clone(),
                                default_filename: data.default_filename.clone(),
                                cwd: None,
                                on_done: move |result: ExportDialogResult| {
                                    on_result(result.message);
                                },
                            )
                        }.into_any())
                    },
                    LocalCommandPanel::Permissions => Some(element! {
                        permissions::PermissionsCommandPanel(
                            on_result: on_permissions_done.clone(),
                            on_message: on_permissions_message,
                        )
                    }.into_any()),
                    LocalCommandPanel::Memory => Some(element! {
                        memory::MemoryCommandPanel(
                            on_close: on_local_command_ui_close,
                            on_result: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Doctor => Some(element! {
                        doctor::Doctor(
                            on_close: on_local_command_ui_close,
                            keybinding_customization_enabled: crate::keybindings::load_user_bindings::is_keybinding_customization_enabled(),
                            keybindings_path: crate::keybindings::load_user_bindings::get_keybindings_path().display().to_string(),
                            keybinding_warnings: crate::keybindings::load_user_bindings::get_cached_keybinding_warnings(),
                        )
                    }.into_any()),
                    LocalCommandPanel::Diff => Some(element! {
                        DiffCommand(
                            messages: Arc::clone(&model_messages),
                            on_done: on_diff_done,
                        )
                    }.into_any()),
                    LocalCommandPanel::Sandbox => Some(element! {
                        SandboxSettings(on_complete: move |result: Option<String>| {
                            if let Some(result) = result {
                                on_local_command_ui_result(result);
                            } else {
                                on_local_command_ui_close(());
                            }
                        })
                    }.into_any()),
                    LocalCommandPanel::Btw { question, context } => {
                        let request_key = format!("btw:{:p}", Arc::as_ptr(&context));
                        Some(element! {
                            View(key: request_key, flex_direction: FlexDirection::Column) {
                                BtwSideQuestion(
                                    question: question,
                                    context: context,
                                    on_done: move |_| on_local_command_ui_close(()),
                                )
                            }
                        }.into_any())
                    }
                    LocalCommandPanel::AddDir { args } => Some(element! {
                        crate::commands::add_dir::add_dir::AddDirCommand(args: args, app_store: Some(app_store.clone()), on_done: on_local_command_ui_result, on_error: move |_| active_local_command_ui.set(None))
                    }.into_any()),
                    LocalCommandPanel::Tasks { context } => Some(element! {
                        crate::components::tasks::background_tasks_dialog::BackgroundTasksDialog(tool_use_context: Some(context), on_done: on_local_command_ui_result)
                    }.into_any()),
                    LocalCommandPanel::Copy(data) => Some(element! {
                        CopyPicker(command_stdout: Some(command_stdout.clone()), data: Some(data), on_done: clipboard_result.clone())
                    }.into_any()),
                    LocalCommandPanel::Mcp { args } => Some(element! {
                        mcp::McpCommandPanel(
                            args: args,
                            on_close: on_local_command_ui_close,
                            on_result: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Login { .. } => Some(element! {
                        crate::commands::login::login::LoginCommand(
                            on_done: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Logout { .. } => Some(element! {
                        crate::commands::logout::logout::LogoutCommand(
                            on_done: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Ide { args } => Some(element! {
                        crate::commands::ide::ide::IdeCommand(
                            args: args,
                            mcp_state: (*app_store.get().mcp).clone(),
                            on_done: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Hooks => Some(element! {
                        HooksCommand(
                            tool_names: hooks_tool_names.clone(),
                            on_done: on_hooks_done,
                        )
                    }.into_any()),
                    LocalCommandPanel::Agents => Some(element! {
                        AgentsCommand(tools: agents_tools.clone(), on_done: on_agents_done)
                    }.into_any()),
                    LocalCommandPanel::Skills => Some(element! {
                        SkillsCommand(on_done: on_skills_done)
                    }.into_any()),
                    LocalCommandPanel::Stats => Some(element! {
                        Stats(on_close: on_local_command_ui_result)
                    }.into_any()),
                    LocalCommandPanel::It2Setup { request } => {
                        let request_for_done = request.clone();
                        let mut active_local_command_ui_for_done = active_local_command_ui;
                        Some(element! {
                            It2SetupPrompt(
                                tmux_available: request.tmux_available,
                                on_done: move |result| {
                                    request_for_done.respond(result);
                                    active_local_command_ui_for_done.set(None);
                                },
                            )
                        }.into_any())
                    }
                }
            }))

            // Maps to: CC `screens/REPL.tsx:6218-6233`. SpinnerWithVerb is a
            // REPL-owned sibling of Messages, never a Messages prop or child.
            // Cometix Special modification: do not pass `verbose` — timer/tokens
            // show immediately (see SpinnerWithVerb).
            #(if show_spinner && screen.get() == Screen::Prompt {
                Some(element! {
                    SpinnerWithVerb(
                        mode: stream_mode.get(),
                        response_length_ref: Some(response_length_ref),
                        spinner_suffix: stop_hook_spinner_suffix_value.clone(),
                        // Maps to: CC `REPL.tsx:6232`
                        // `hasActiveTools={inProgressToolUseIDs.size > 0}`.
                        has_active_tools: !in_progress_tool_use_ids_value.is_empty(),
                        leader_is_idle: !is_loading,
                    )
                })
            } else {
                None
            })
            // Preserve the inline-mode gap that followed the old spinner row;
            // this is an iocraft transport spacer, not Messages ownership.
            #(if show_spinner && screen.get() == Screen::Prompt {
                Some(element! { View(height: 1u32) {} })
            } else {
                None
            })

            // Immediate local command UI maps to the native scrollback equivalent of
            // the official bottom slot: it stays after scrollable content and
            // before PromptInput when PromptInput is allowed to remain mounted.
            #(active_local_command_ui_snapshot.clone().and_then(|active| {
                if !active.is_immediate {
                    return None;
                }
                let clipboard_result = on_clipboard_command_result(active.clone());
                match active.panel {
                    LocalCommandPanel::PluginSettings { data, .. } => Some(element! {
                        crate::commands::plugin::plugin_settings::PluginSettings(args: data.args, on_complete: plugin_on_complete.clone())
                    }.into_any()),
                    LocalCommandPanel::Settings(tab) => Some(element! {
                        Settings(
                            on_close: on_local_command_ui_close,
                            on_result: on_local_command_ui_result,
                            default_tab: Some(tab),
                            session_id: current_resume_session_id.clone(),
                            session_name: current_resume_session_name.clone(),
                        )
                    }.into_any()),
                    LocalCommandPanel::Resume => Some(element! {
                        View(flex_direction: FlexDirection::Column) {
                            ClearLiveOutputOnMount
                            ResumeCommand(
                                command_stdout: Some(command_stdout.clone()),
                                current_session_id: current_resume_session_id.clone(),
                                on_close: { let done = clipboard_result.clone(); move |()| done("Resume cancelled".to_string()) },
                                on_select: {
                                    let on_resume_select = on_resume_select.clone();
                                    let active = active.clone();
                                    move |selection| on_resume_select(active.clone(), selection)
                                },
                                on_result: clipboard_result.clone(),
                            )
                        }
                    }.into_any()),
                    LocalCommandPanel::Help => Some(element! {
                        HelpV2(on_close: on_local_command_ui_close, commands: Some(commands.clone()))
                    }.into_any()),
                    LocalCommandPanel::Model => Some(element! {
                        model::ModelPickerWrapper(
                            on_close: on_local_command_ui_close,
                            on_select: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Effort {
                        args,
                        has_conversation_messages,
                    } => Some(element! {
                        EffortPicker(
                            args: args.clone(),
                            has_conversation_messages: has_conversation_messages,
                            on_close: on_local_command_ui_close,
                            on_cancel_args: on_local_command_ui_result.clone(),
                            on_select: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Fast => {
                        let initial_enabled = app_store.get().fast_mode;
                        let mut on_result = on_local_command_ui_result.clone();
                        let mut on_close = on_local_command_ui_close.clone();
                        let store_for_fast = app_store.clone();
                        // Maps to: CC fast.tsx:252-258 — read the real
                        // unavailable reason before mounting the picker. CC's
                        // `await prefetchFastModeStatus()` (:238) resolves the
                        // org status first; Rust reads the synchronously
                        // cached org status, same as the PromptInput mount.
                        // P4 review fix (2026-08-02).
                        let fast_unavailable_reason =
                            crate::utils::fast_mode::get_fast_mode_unavailable_reason();
                        Some(element! {
                            fast::FastModePicker(
                                initial_enabled: initial_enabled,
                                unavailable_reason: fast_unavailable_reason,
                                on_done: move |choice: Option<bool>| {
                                    match choice {
                                        Some(enabled) => {
                                            // Maps to: CC fast.tsx:35-58 applyFastMode —
                                            // clearFastModeCooldown (:39), persist to
                                            // userSettings (:40-42, `fastMode: enable ?
                                            // true : undefined`; Null carries the
                                            // undefined delete), and the ON-branch model
                                            // switch when the current model lacks
                                            // fast-mode support (:44-53). Realized
                                            // 2026-08-02 (P4 batch).
                                            crate::utils::fast_mode::clear_fast_mode_cooldown();
                                            let _ = crate::utils::settings::update_settings_for_source(
                                                crate::utils::settings::SettingSource::User,
                                                &serde_json::Map::from_iter([(
                                                    "fastMode".to_string(),
                                                    if enabled {
                                                        serde_json::Value::Bool(true)
                                                    } else {
                                                        serde_json::Value::Null
                                                    },
                                                )]),
                                            );
                                            store_for_fast.replace_with(|state| {
                                                if enabled
                                                    && !crate::utils::fast_mode::is_fast_mode_supported_by_model(
                                                        state.main_loop_model.as_deref(),
                                                    )
                                                {
                                                    state.main_loop_model =
                                                        Some(crate::utils::fast_mode::get_fast_mode_model());
                                                    state.main_loop_model_for_session = None;
                                                }
                                                state.fast_mode = enabled;
                                            });
                                            let msg = if enabled {
                                                "Fast mode ON".to_string()
                                            } else {
                                                "Fast mode OFF".to_string()
                                            };
                                            on_result(msg);
                                        }
                                        None => on_close(()),
                                    }
                                },
                            )
                        }.into_any())
                    },
                    LocalCommandPanel::Theme => Some(element! {
                        theme::ThemePickerWrapper(
                            on_close: on_local_command_ui_close,
                            on_select: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Export(data, _) => {
                        let on_result = clipboard_result.clone();
                        Some(element! {
                            ExportDialog(
                                command_stdout: Some(command_stdout.clone()),
                                content: data.content.clone(),
                                default_filename: data.default_filename.clone(),
                                cwd: None,
                                on_done: move |result: ExportDialogResult| {
                                    on_result(result.message);
                                },
                            )
                        }.into_any())
                    },
                    LocalCommandPanel::Permissions => Some(element! {
                        permissions::PermissionsCommandPanel(
                            on_result: on_permissions_done.clone(),
                            on_message: on_permissions_message,
                        )
                    }.into_any()),
                    LocalCommandPanel::Memory => Some(element! {
                        memory::MemoryCommandPanel(
                            on_close: on_local_command_ui_close,
                            on_result: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Doctor => Some(element! {
                        doctor::Doctor(
                            on_close: on_local_command_ui_close,
                            keybinding_customization_enabled: crate::keybindings::load_user_bindings::is_keybinding_customization_enabled(),
                            keybindings_path: crate::keybindings::load_user_bindings::get_keybindings_path().display().to_string(),
                            keybinding_warnings: crate::keybindings::load_user_bindings::get_cached_keybinding_warnings(),
                        )
                    }.into_any()),
                    LocalCommandPanel::Diff => Some(element! {
                        DiffCommand(
                            messages: Arc::clone(&model_messages),
                            on_done: on_diff_done,
                        )
                    }.into_any()),
                    LocalCommandPanel::Sandbox => Some(element! {
                        SandboxSettings(on_complete: move |result: Option<String>| {
                            if let Some(result) = result {
                                on_local_command_ui_result(result);
                            } else {
                                on_local_command_ui_close(());
                            }
                        })
                    }.into_any()),
                    LocalCommandPanel::Btw { question, context } => {
                        let request_key = format!("btw:{:p}", Arc::as_ptr(&context));
                        Some(element! {
                            View(key: request_key, flex_direction: FlexDirection::Column) {
                                BtwSideQuestion(
                                    question: question,
                                    context: context,
                                    on_done: move |_| on_local_command_ui_close(()),
                                )
                            }
                        }.into_any())
                    }
                    LocalCommandPanel::AddDir { args } => Some(element! {
                        crate::commands::add_dir::add_dir::AddDirCommand(args: args, app_store: Some(app_store.clone()), on_done: on_local_command_ui_result, on_error: move |_| active_local_command_ui.set(None))
                    }.into_any()),
                    LocalCommandPanel::Tasks { context } => Some(element! {
                        crate::components::tasks::background_tasks_dialog::BackgroundTasksDialog(tool_use_context: Some(context), on_done: on_local_command_ui_result)
                    }.into_any()),
                    LocalCommandPanel::Copy(data) => Some(element! {
                        CopyPicker(command_stdout: Some(command_stdout.clone()), data: Some(data), on_done: clipboard_result.clone())
                    }.into_any()),
                    LocalCommandPanel::Mcp { args } => Some(element! {
                        mcp::McpCommandPanel(
                            args: args,
                            on_close: on_local_command_ui_close,
                            on_result: on_local_command_ui_result,
                        )
                    }.into_any()),
                    LocalCommandPanel::Hooks => Some(element! {
                        HooksCommand(
                            tool_names: hooks_tool_names.clone(),
                            on_done: on_hooks_done,
                        )
                    }.into_any()),
                    LocalCommandPanel::Agents => Some(element! {
                        AgentsCommand(tools: agents_tools.clone(), on_done: on_agents_done)
                    }.into_any()),
                    LocalCommandPanel::Skills => Some(element! {
                        SkillsCommand(on_done: on_skills_done)
                    }.into_any()),
                    LocalCommandPanel::Stats => Some(element! {
                        Stats(on_close: on_local_command_ui_result)
                    }.into_any()),
                    // These commands are regular bottom-slot dialogs. They
                    // are mounted by the non-immediate branch above so they
                    // do not get duplicated in the immediate slot.
                    LocalCommandPanel::Login { .. }
                    | LocalCommandPanel::Logout { .. }
                    | LocalCommandPanel::Ide { .. } => None,
                    LocalCommandPanel::It2Setup { .. } => None,
                }
            }))

            #(match active_startup_dialog {
                Some(ReplStartupDialogKind::RemoteCallout) => Some(element! {
                    RemoteCallout(on_done: on_remote_callout_done)
                }.into_any()),
                Some(ReplStartupDialogKind::PluginHint) => hint_recommendation_snapshot.clone().map(|recommendation| element! {
                    crate::components::claude_code_hint::PluginHintMenu(
                        plugin_name: recommendation.plugin_name,
                        plugin_description: recommendation.plugin_description,
                        marketplace_name: recommendation.marketplace_name,
                        source_command: recommendation.source_command,
                        on_response: on_plugin_hint_response,
                    )
                }.into_any()),
                Some(ReplStartupDialogKind::DesktopUpsell) => Some(element! {
                    DesktopUpsellStartup(
                        handoff_state: startup_dialog_snapshot.desktop_handoff_state,
                        handoff_error: startup_dialog_snapshot.desktop_handoff_error.clone(),
                        handoff_download_message: startup_dialog_snapshot.desktop_handoff_download_message.clone(),
                        on_done: on_desktop_upsell_done,
                    )
                }.into_any()),
                None => None,
            })

            #(if exit_flow_active_snapshot {
                Some(element! {
                    ExitFlow(
                        show_worktree: false,
                        on_done: on_exit_flow_done,
                        on_cancel: on_exit_flow_cancel,
                    )
                }.into_any())
            } else {
                None
            })

            // Maps to: CC REPL.tsx:6876 `<MessageSelector messages={...}/>`
            // (rewind dialog; /rewind command or empty-input double-Escape).
            #(if message_selector_visible_snapshot {
                let selector_messages = Arc::clone(&model_messages);
                let store_for_restore_code = app_store.clone();
                let mut message_selector_visible_for_close = message_selector_visible;
                Some(element! {
                    crate::components::message_selector::MessageSelector(
                        messages: selector_messages,
                        on_pre_restore: move |_| on_cancel(),
                        on_summarize: Some(Arc::new(move |uuid: String, feedback: Option<String>, direction| -> futures::future::BoxFuture<'static, Result<(), String>> {
                            let (sender, receiver) = futures::channel::oneshot::channel();
                            handle_summarize((uuid, feedback, direction, sender));
                            Box::pin(async move { receiver.await.map_err(|error| error.to_string())? })
                        }) as crate::components::message_selector::SummarizeCallback),
                        on_restore_code: Some(Arc::new(move |uuid: String| -> futures::future::BoxFuture<'static, Result<(), String>> {
                            let store = store_for_restore_code.clone();
                            Box::pin(async move {
                                crate::utils::file_history::file_history_rewind(&store, &uuid).await
                            })
                        }) as crate::components::message_selector::RestoreCodeCallback),
                        on_restore_message: Some(Arc::new(move |uuid: String| -> futures::future::BoxFuture<'static, Result<(), String>> {
                            handle_restore_message(uuid);
                            Box::pin(async { Ok(()) })
                        }) as crate::components::message_selector::RestoreCodeCallback),
                        on_close: move |_| {
                            message_selector_visible_for_close.set(false);
                        },
                        columns: 0usize,
                    )
                }.into_any())
            } else {
                None
            })

            #(active_prompt_shell_command_snapshot.map(|command| element! {
                BashModeProgress(
                    input: command,
                    progress: None,
                    verbose: verbose,
                )
            }.into_any()))

            #(transcript_footer_text.map(|text| element! {
                View(
                    margin_top: 1u32,
                    padding_left: 2u32,
                    border_style: BorderStyle::Single,
                    border_edges: Edges::Top,
                ) {
                    Text(content: text, dim: true)
                }
            }))

            #(if should_render_prompt_input {
                Some(element! {
                    Fragment {
                    PromptInput(
                        // SEAM (slice 3b review A3): this per-submit key remounts
                        // PromptInput and the entire auto-updater subtree —
                        // child check loops re-fire and child-local display
                        // state resets on every submit, unlike CC's
                        // session-persistent PromptInput. Invisible today only
                        // because updater IO is short-circuited; lifting the
                        // updater subtree out of this keyed region is a hard
                        // prerequisite for real update-install IO.
                        key: format!("prompt-{}", prompt_input_generation.get()),
                        debug: debug,
                        // Maps to: CC REPL.tsx:6816 `ideSelection={ideSelection}`.
                        ide_selection: ide_selection.read().clone(),
                        api_key_status: api_key_status,
                        // Maps to: CC REPL.tsx:6830 `autoUpdaterResult={autoUpdaterResult}`.
                        auto_updater_result: auto_updater_result.read().clone(),
                        // Maps to: CC REPL.tsx:6829 `onAutoUpdaterResult={setAutoUpdaterResult}`
                        // + REPL.tsx:1411-1421 `useEffect([autoUpdaterResult])` notifications
                        // fan-out. CC's effect fires exactly once per committed result identity
                        // (this setter is the only writer, always with a fresh value), so
                        // running the fan-out after the commit here is the same observable
                        // sequence without a retained-mode effect carrier.
                        on_auto_updater_result: Handler::from({
                            let app_store = app_store.clone();
                            move |result: crate::utils::auto_updater::AutoUpdaterResult| {
                                let mut auto_updater_result = auto_updater_result;
                                auto_updater_result.set(Some(result.clone()));
                                // CC REPL.tsx:1413-1420: one addNotification
                                // ({key: 'auto-updater-notification', priority:
                                // 'low'}) per entry. Dormant on both sides —
                                // no CC 2.1.88 producer populates
                                // `AutoUpdaterResult.notifications`.
                                let mut notifications =
                                    crate::context::notifications::NotificationsWriter::new(app_store.clone());
                                for text in &result.notifications {
                                    notifications.add_notification(
                                        crate::context::notifications::Notification::text(
                                            "auto-updater-notification",
                                            text.clone(),
                                            crate::context::notifications::NotificationPriority::Low,
                                        ),
                                    );
                                }
                            }
                        }),
                        // Maps to: CC REPL → PromptInput.tsx:237 `messages`
                        // (model-visible history; memoized projection Arc).
                        messages: Arc::clone(&model_messages),
                        commands: Some(commands.clone()),
                        initial_input: Some(prompt_input_snapshot.read().clone()),
                        controlled_input: restored_input.read().clone(),
                        on_input_state_change: move |next: crate::components::prompt_input::PromptInputTextUpdate| {
                            let mut restored_input = restored_input;
                            if restored_input.read().as_ref() != Some(&next) {
                                restored_input.set(Some(next));
                            }
                        },
                        on_input_change: move |next: String| {
                            let mut prompt_input_snapshot = prompt_input_snapshot;
                            if *prompt_input_snapshot.read() != next {
                                // Maps to: CC REPL.tsx:1855
                                // `setIsPromptInputActive(value.trim().length > 0)`
                                // plus the REPL.tsx:1862-1870 timer reset keyed
                                // on inputValue changes.
                                let mut is_prompt_input_active = is_prompt_input_active;
                                let mut prompt_input_last_change = prompt_input_last_change;
                                is_prompt_input_active.set(!next.trim().is_empty());
                                prompt_input_last_change.set(Instant::now());
                                prompt_input_snapshot.set(next);
                            }
                        },
                        on_modal_overlay_change: move |active: bool| {
                            if prompt_modal_overlay_active.get() != active {
                                prompt_modal_overlay_active.set(active);
                            }
                        },
                        on_submit: on_submit,
                        on_exit: on_exit,
                        // Maps to: CC REPL.tsx:4895 handleShowMessageSelector
                        // — setIsMessageSelectorVisible(prev => !prev).
                        on_show_message_selector: move |_: ()| {
                            let mut message_selector_visible = message_selector_visible;
                            message_selector_visible.set(!message_selector_visible.get());
                        },
                        is_local_command_ui_active: is_local_command_ui_active_for_prompt,
                        is_loading: is_loading,
                        has_assistant_messages: messages.iter().any(|message| matches!(message.kind, RenderableMessageKind::Assistant { .. })),
                        permission_mode: permission_store.tool_permission_context().mode,
                        swarm_banner_input: swarm_banner_input.clone(),
                        on_permission_mode_cycle: on_permission_mode_cycle,
                    )
                    // CC REPL.tsx:6865: same visibility/lifetime as PromptInput,
                    // after it in sibling order; never a global Task context.
                    crate::components::session_background_hint::SessionBackgroundHint(
                        is_loading: is_loading,
                    )
                    }
                }.into_any())
            } else {
                None
            })
            }
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    struct BranchFixtureDirectory(std::path::PathBuf);
    impl BranchFixtureDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("cometix-branch-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for BranchFixtureDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    use super::*;
    use crate::utils::env_utils::EnvVarGuard;
    use crate::utils::{conversation, conversation_recovery, theme};
    use futures::{Stream, StreamExt, stream};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone)]
    pub(super) struct ReplActiveQueryProbe {
        pub(super) handle: QueryHandle,
        pub(super) start_active: bool,
        pub(super) started_queries: Arc<Mutex<Vec<crate::query::QueryParams>>>,
        pub(super) snapshot: Arc<Mutex<Option<ReplActiveQuerySnapshot>>>,
    }

    pub(super) struct ReplActiveQuerySnapshot {
        pub(super) rows: Vec<RenderableMessage>,
        pub(super) model_messages: Vec<Message>,
        pub(super) query_id: Option<String>,
        pub(super) local_command_open: bool,
    }

    fn append_resume_hook_marker_command(path: &std::path::Path, marker: &str) -> String {
        #[cfg(windows)]
        {
            format!("echo {marker}>>\"{}\"", path.display())
        }
        #[cfg(not(windows))]
        {
            let path = path.display().to_string().replace('\'', "'\\''");
            format!(
                "printf '%s\\n' '{marker}' >> '{path}'; printf '%s' '{{\"systemMessage\":\"{marker} hook message\"}}'"
            )
        }
    }

    fn recording_sandbox_ask(
        host: &str,
        tag: &'static str,
        events: &Arc<Mutex<Vec<String>>>,
    ) -> LocalSandboxPermissionAsk {
        let events = Arc::clone(events);
        LocalSandboxPermissionAsk {
            host_pattern: crate::utils::sandbox::sandbox_adapter::NetworkHostPattern::new(host),
            resolve: Arc::new(move |allow| {
                events
                    .lock()
                    .unwrap()
                    .push(format!("resolve:{tag}:{allow}"));
            }),
        }
    }

    fn sandbox_response(
        allow: bool,
        persist_to_settings: bool,
    ) -> crate::components::permissions::sandbox_permission_request::SandboxPermissionResponse {
        crate::components::permissions::sandbox_permission_request::SandboxPermissionResponse {
            allow,
            persist_to_settings,
        }
    }

    /// Maps to: CC replLauncher.tsx:24-26 props spread and REPL.tsx:6134-6138 manager.
    #[test]
    fn repl_props_into_element_preserves_mcp_startup_and_runs_non_strict_discovery() {
        let _env = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_runtime::initialize_test_process_runtime();
        let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work().unwrap();
        let _runtime = runtime.enter();
        let directory = BranchFixtureDirectory::new();
        let config = directory.path().join("config");
        let managed = directory.path().join("managed");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let _remote = EnvVarGuard::set("ENABLE_CLAUDEAI_MCP_SERVERS", "0");
        let _cwd = OriginalCwdGuard::set(directory.path());
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::utils::plugins::plugin_loader::clear_plugin_cache(None);
        let _auth = ScopedReplAuthConfig::disable_anthropic_auth();
        let name = format!("entry-fixture-{}", uuid::Uuid::new_v4());
        let server = crate::services::mcp::types::ScopedMcpServerConfig::from_config(
            crate::services::mcp::types::ConfigScope::Dynamic,
            &crate::utils::config::McpServerConfig {
                server_type: Some("sdk".into()),
                name: Some(name.clone()),
                ..Default::default()
            },
        );
        let startup = Arc::new(crate::main::McpStartupConfig {
            dynamic: indexmap::IndexMap::from([(name.clone(), server.clone())]),
            strict: false,
            bare: false,
        });
        let props = ReplProps {
            mcp_startup: Some(startup.clone()),
            ..Default::default()
        };
        assert!(Arc::ptr_eq(props.mcp_startup.as_ref().unwrap(), &startup));
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        assert!(
            store.get().mcp.clients.is_empty(),
            "no main.rs pending seed: only the mounted effect can add this server"
        );
        let mut app = element! {
            ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    crate::state::app_state::AppStateProvider(
                        prebuilt_store: Some(store.clone()),
                        children: crate::state::app_state::ProviderChildren::new(move || props.clone().into_element()),
                    )
                }
            }
        };
        futures::executor::block_on(async {
            let mut render = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::pending::<TerminalEvent>())
                        .with_size(100, 30),
                ),
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(4);
            let mut last_canvas = String::new();
            loop {
                if let Some(canvas) = crate::utils::race(render.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(10)).await;
                    None
                })
                .await
                {
                    last_canvas = canvas.to_string();
                }
                let current = store.get();
                if let Some(client) = current.mcp.clients.iter().find(|client| {
                    client.client.name == name
                        && client.client.status
                            == crate::services::mcp::types::McpServerConnectionType::Failed
                }) {
                    assert_eq!(
                        client.config.as_ref(),
                        Some(&server),
                        "actual manager must receive the launcher config"
                    );
                    // Native SDK transport is owned externally and rejects a
                    // standalone connection deterministically. Await that final
                    // callback (not transient pending) to prove both effects ran,
                    // without a network service, subprocess, or model request.
                    assert_eq!(
                        client.client.status,
                        crate::services::mcp::types::McpServerConnectionType::Failed
                    );
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "ReplProps→into_element→manager never initialized its server; canvas={last_canvas}"
                );
            }
        });
        crate::utils::plugins::plugin_loader::clear_plugin_cache(None);
        crate::utils::settings::settings_cache::reset_settings_cache();
    }

    /// Maps to: CC REPL.tsx:6332-6343 — all pending same-host asks resolve
    /// exactly once with the user's answer; other hosts stay queued in FIFO
    /// order for the next dialog.
    #[test]
    fn local_sandbox_response_fans_out_same_host_and_retains_others_fifo() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut queue = vec![
            recording_sandbox_ask("a.example.com", "a1", &events),
            recording_sandbox_ask("b.example.com", "b1", &events),
            recording_sandbox_ask("a.example.com", "a2", &events),
        ];
        handle_local_sandbox_permission_response(
            &mut queue,
            sandbox_response(true, false),
            &mut |_, _| panic!("persist must not run when persistToSettings is false"),
        );
        assert_eq!(
            events.lock().unwrap().clone(),
            vec!["resolve:a1:true", "resolve:a2:true"],
            "same-host asks resolve exactly once each"
        );
        assert_eq!(queue.len(), 1, "other hosts stay queued");
        assert_eq!(queue[0].host_pattern.host, "b.example.com");

        // FIFO: the retained entry is next.
        handle_local_sandbox_permission_response(
            &mut queue,
            sandbox_response(false, false),
            &mut |_, _| panic!("persist must not run when persistToSettings is false"),
        );
        assert_eq!(
            events.lock().unwrap().last().map(String::as_str),
            Some("resolve:b1:false")
        );
        assert!(queue.is_empty());
    }

    /// Maps to: CC REPL.tsx:6302-6343 — the persistToSettings branch runs
    /// before resolution (apply + persist + refreshConfig precede
    /// `resolvePromise`), and the answer resolves unchanged (CC
    /// persistPermissionUpdate is fire-and-forget).
    #[test]
    fn local_sandbox_response_persist_precedes_resolve() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut queue = vec![recording_sandbox_ask("a.example.com", "a1", &events)];
        let persist_events = Arc::clone(&events);
        handle_local_sandbox_permission_response(
            &mut queue,
            sandbox_response(true, true),
            &mut |host, allow| {
                persist_events
                    .lock()
                    .unwrap()
                    .push(format!("persist:{host}:{allow}"));
            },
        );
        assert_eq!(
            events.lock().unwrap().clone(),
            vec!["persist:a.example.com:true", "resolve:a1:true"]
        );
        assert!(queue.is_empty());
    }

    /// Maps to: CC REPL.tsx:6297-6298 `if (!currentRequest) return`.
    #[test]
    fn local_sandbox_response_with_empty_queue_is_noop() {
        let mut queue: Vec<LocalSandboxPermissionAsk> = Vec::new();
        handle_local_sandbox_permission_response(
            &mut queue,
            sandbox_response(true, true),
            &mut |_, _| panic!("persist must not run without a current request"),
        );
        assert!(queue.is_empty());
    }

    /// `cargo build`, not `echo`: `echo` is semantically neutral
    /// (`bash_tool/mod.rs#BASH_SEMANTIC_NEUTRAL_COMMANDS`) and allows outright,
    /// which would make the "still asking" premise of a race test vacuous.
    fn racing_bash_request(tool_use_id: &str) -> crate::types::permissions::PermissionRequest {
        crate::types::permissions::PermissionRequest {
            permission_result: None,
            id: format!("perm-{tool_use_id}"),
            tool_use_id: tool_use_id.to_string(),
            tool_name: "Bash".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "Run command?".to_string(),
            message: String::new(),
            input_summary: "cargo build".to_string(),
            input: json!({ "command": "cargo build" }),
            call_input: None,
            rule: crate::types::permissions::PermissionRuleValue::new(
                "Bash",
                Some("cargo build".to_string()),
            ),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: crate::types::permissions::PermissionMode::Default,
        }
    }

    /// The dialog answer under test: "yes, and don't ask again", whose
    /// `permissionUpdates` reach `settings.local.json` AND the live context
    /// (`PermissionContext.ts:139-147`). Both effects are what a losing racer
    /// must not produce.
    fn always_allow_with_a_persisted_rule() -> PermissionPromptResponse {
        let mut response = PermissionPromptResponse::new(PermissionPromptChoice::AlwaysAllow);
        response.permission_updates = vec![crate::types::permissions::PermissionUpdate::AddRules {
            destination: crate::types::permissions::PermissionUpdateDestination::LocalSettings,
            behavior: crate::types::permissions::PermissionBehavior::Allow,
            rules: vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash",
                Some("cargo build".to_string()),
            )],
        }];
        response.permission_updates_explicit = true;
        response
    }

    struct RacingPermissionFixture {
        root: PathBuf,
        _cwd: OriginalCwdGuard,
        _config: EnvVarGuard,
        _write: EnvVarGuard,
        store: crate::state::store::AppStore,
        leader: crate::utils::swarm::leader_permission_bridge::TestLeaderQueue,
        row: ToolUseConfirm,
        // Held so the row's promise still has a waiter: a dropped receiver is
        // an abandoned ask, which is a different case from a lost race.
        _waiter: async_channel::Receiver<PermissionPromptResponse>,
    }

    /// A queue holding one row that is genuinely asking, a context in which the
    /// sweep's re-evaluation now says `allow`, and a writable project root so
    /// the persist half of an answer is observable on disk.
    fn racing_permission_fixture(name: &str) -> RacingPermissionFixture {
        let root = std::env::temp_dir().join(format!("cometix-repl-permission-race-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // macOS exposes the temp root through `/var` -> `/private/var`.
        let root = std::fs::canonicalize(&root).unwrap();
        let _cwd = OriginalCwdGuard::set(&root);
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root.join("config"));
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", &PathBuf::from("1"));

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        // What answering some OTHER prompt with "don't ask again" leaves
        // behind, and what makes the sweep's `hasPermissionsToUseTool` flip to
        // `allow` (CC REPL.tsx:3114-3116).
        let mut granted = store.tool_permission_context();
        granted.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash", None,
            )],
        );
        store.set_tool_permission_context(granted);

        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let (response_tx, _waiter) = async_channel::bounded(1);
        let row = ToolUseConfirm::new(racing_bash_request("toolu_race")).with_responder(
            crate::types::permissions::PermissionPromptResponder::new(response_tx),
        );
        leader.setter.push_to_queue(row.clone());

        RacingPermissionFixture {
            root,
            _cwd,
            _config,
            _write,
            store,
            leader,
            row,
            _waiter,
        }
    }

    fn persisted_local_rules(fixture: &RacingPermissionFixture) -> Option<serde_json::Value> {
        let settings =
            std::fs::read_to_string(fixture.root.join(".claude/settings.local.json")).ok()?;
        serde_json::from_str::<serde_json::Value>(&settings)
            .ok()
            .map(|settings| settings["permissions"]["allow"].clone())
    }

    fn live_context_has_local_rules(fixture: &RacingPermissionFixture) -> bool {
        fixture
            .store
            .tool_permission_context()
            .always_allow_rules
            .get(&crate::types::permissions::PermissionRuleSource::LocalSettings)
            .is_some_and(|rules| !rules.is_empty())
    }

    /// Maps to: CC `interactiveHandler.ts:160` `if (!claim()) return // atomic
    /// check-and-mark before await` — the answer that lost the race to
    /// `recheckPermission` (`:222` claim, `:227` withdraw, `:229` resolve)
    /// returns before `ctx.handleUserAllow` and therefore before
    /// `ctx.persistPermissions` (`PermissionContext.ts:299-300`, `:139-147`).
    ///
    /// OLD SHAPE: `on_permission_response` ran `persist_permissions(...)`
    /// inline, ahead of every look at the entry's `responder`, so the losing
    /// answer wrote `Bash(cargo build)` into `settings.local.json` and moved the
    /// live permission context on its way to a `respond()` the winner had
    /// already made a no-op. Both assertions below failed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dialog_answer_that_lost_the_claim_persists_nothing_and_applies_nothing() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let fixture = racing_permission_fixture("lost");

        // Racer 1: the sweep, which claims, withdraws and resolves.
        assert!(
            crate::hooks::tool_permission::handlers::interactive_handler::recheck_permission(
                &fixture.row,
                &fixture.store,
                Some(fixture.leader.setter.clone()),
            )
            .await,
            "the granted rule must make the recheck resolve this row"
        );
        assert!(fixture.leader.queue.lock().unwrap().is_empty());

        // Racer 2: the user's "yes, don't ask again", a beat too late.
        assert!(
            matches!(
                claim_permission_answer(
                    &fixture.row,
                    always_allow_with_a_persisted_rule(),
                    &fixture.store,
                ),
                PermissionAnswerClaim::Lost
            ),
            "the second answer must lose the claim"
        );

        assert_eq!(
            persisted_local_rules(&fixture),
            None,
            "a lost answer must not reach settings.local.json"
        );
        assert!(
            !live_context_has_local_rules(&fixture),
            "a lost answer must not move the live permission context"
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    /// The other direction of the same race, pinned deterministically: the
    /// dialog answer takes the claim first, and the sweep arriving a beat later
    /// must do nothing at all.
    ///
    /// Maps to: CC `interactiveHandler.ts:222` `if (!claim()) return` inside
    /// `recheckPermission` — the comment there names this exact window ("the
    /// async hasPermissionsToUseTool call above opens a window where CCR could
    /// have responded in flight"), and the loser must not reach `:227`
    /// `ctx.removeFromQueue()` or `:229` `resolveOnce(...)`.
    ///
    /// OLD SHAPE: `recheck_permission` had no `claim()`; it inferred winning
    /// from `responder.respond(...)`, so it still withdrew the row and, on a
    /// `bounded(1)` slot the waiter had already drained, could deliver a second
    /// decision.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recheck_that_lost_the_claim_neither_withdraws_nor_resolves() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let fixture = racing_permission_fixture("recheck-lost");

        // Racer 1: the user's answer, which claims and persists.
        assert!(matches!(
            claim_permission_answer(
                &fixture.row,
                always_allow_with_a_persisted_rule(),
                &fixture.store,
            ),
            PermissionAnswerClaim::Won(_)
        ));

        // Racer 2: the sweep, whose fresh evaluation says `allow` but whose
        // claim is gone.
        assert!(
            !crate::hooks::tool_permission::handlers::interactive_handler::recheck_permission(
                &fixture.row,
                &fixture.store,
                Some(fixture.leader.setter.clone()),
            )
            .await,
            "a recheck that lost the claim reports no decision"
        );
        assert_eq!(
            fixture.leader.queue.lock().unwrap().len(),
            1,
            "the loser must not withdraw the row it does not own"
        );
        assert!(
            fixture._waiter.try_recv().is_err(),
            "the loser must not resolve the promise the winner owns"
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    /// The same two racers with no ordering imposed: both start behind a
    /// barrier and whichever takes the claim first wins. CC's invariant is that
    /// `claimed` admits exactly one, and that the loser leaves no trace — so the
    /// side effects must match the winner, not the wall clock.
    ///
    /// This one is the mutual-exclusion check; the two directions above are
    /// where the ordering itself is pinned. Its old-shape failure is
    /// probabilistic by construction: `recheck_permission` awaits
    /// `has_permissions_to_use_tool_async` while the dialog path is
    /// synchronous, so the dialog usually reaches the claim first and the
    /// old-shape run looks identical.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_dialog_and_the_recheck_race_and_only_the_winners_effects_land() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let fixture = racing_permission_fixture("concurrent");
        let start = Arc::new(std::sync::Barrier::new(2));

        let sweep = tokio::spawn({
            let row = fixture.row.clone();
            let store = fixture.store.clone();
            let setter = fixture.leader.setter.clone();
            let start = Arc::clone(&start);
            async move {
                start.wait();
                crate::hooks::tool_permission::handlers::interactive_handler::recheck_permission(
                    &row,
                    &store,
                    Some(setter),
                )
                .await
            }
        });
        let dialog = tokio::task::spawn_blocking({
            let row = fixture.row.clone();
            let store = fixture.store.clone();
            let start = Arc::clone(&start);
            move || {
                start.wait();
                matches!(
                    claim_permission_answer(&row, always_allow_with_a_persisted_rule(), &store),
                    PermissionAnswerClaim::Won(_)
                )
            }
        });

        let sweep_won = sweep.await.expect("sweep task");
        let dialog_won = dialog.await.expect("dialog task");
        assert!(
            sweep_won ^ dialog_won,
            "exactly one racer may take the claim (sweep={sweep_won} dialog={dialog_won})"
        );

        if dialog_won {
            assert_eq!(
                persisted_local_rules(&fixture),
                Some(json!(["Bash(cargo build)"])),
                "the winning answer persists its updates"
            );
            assert!(live_context_has_local_rules(&fixture));
        } else {
            assert_eq!(
                persisted_local_rules(&fixture),
                None,
                "the losing answer must leave settings.local.json untouched"
            );
            assert!(!live_context_has_local_rules(&fixture));
        }
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    /// Maps to: CC `inProcessRunner.ts:263-281` — an in-process teammate's
    /// `onAllow` persists to disk and then hands the leader
    /// `setToolPermissionContext(updatedContext, { preserveMode: true })`,
    /// "to prevent workers' transformed 'acceptEdits' context from leaking back
    /// to the coordinator" — contrasted with `interactiveHandler.ts:172-181` →
    /// `PermissionContext.ts:139-147`, which persists AND applies with no
    /// options because "user-initiated mode changes (e.g., selecting 'allow all
    /// edits') must NOT be overridden" (`REPL.tsx:3105-3107`).
    ///
    /// Both legs run the production pair in the production order —
    /// `claim_permission_answer` (the disk write) then
    /// `apply_permission_answer_to_leader_context` (the context write) — because
    /// the bug this pins lives BETWEEN them. A test that drove
    /// `set_leader_tool_permission_context` directly cannot see it: the setter
    /// is correct in isolation
    /// (`preserve_mode_keeps_the_leaders_mode_and_takes_every_other_field`).
    ///
    /// OLD SHAPE: the claim ran `persist_permissions`, which projects into the
    /// leader's store, for EVERY row. By the time `preserveMode` read
    /// `prev.toolPermissionContext.mode` that mode was already the teammate's,
    /// so the preserve was a no-op and the first assertion below failed with
    /// `AcceptEdits` — the exact leak 73eeb6b's preserveMode work existed to
    /// prevent.
    #[test]
    fn a_teammate_answer_persists_its_rule_without_moving_the_leaders_mode() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let fixture = racing_permission_fixture("preserve-mode");
        assert_eq!(
            fixture.store.tool_permission_context().mode,
            crate::types::permissions::PermissionMode::Default
        );

        // "Yes, and don't ask again" answered on a teammate's row, carrying the
        // mode switch an ExitPlanMode-style dialog attaches.
        let answer_with_a_mode_switch = || {
            let mut response = always_allow_with_a_persisted_rule();
            response.permission_updates.push(
                crate::types::permissions::PermissionUpdate::SetMode {
                    destination: crate::types::permissions::PermissionUpdateDestination::Session,
                    mode: crate::types::permissions::PermissionMode::AcceptEdits,
                },
            );
            response
        };

        let (teammate_tx, _teammate_waiter) = async_channel::bounded(1);
        let mut teammate = ToolUseConfirm::new(racing_bash_request("toolu_teammate"))
            .with_responder(crate::types::permissions::PermissionPromptResponder::new(
                teammate_tx,
            ));
        teammate.source = crate::types::permissions::PermissionRowSource::InProcessTeammate;

        let PermissionAnswerClaim::Won(response) =
            claim_permission_answer(&teammate, answer_with_a_mode_switch(), &fixture.store)
        else {
            panic!("the only racer must win the claim");
        };
        let effective_request = response.apply_to_request(teammate.request.clone());
        apply_permission_answer_to_leader_context(
            &teammate,
            &fixture.store,
            &effective_request,
            &response,
        );

        assert_eq!(
            fixture.store.tool_permission_context().mode,
            crate::types::permissions::PermissionMode::Default,
            "CC `:277-279` `{{ preserveMode: true }}` — a teammate's setMode must not \
             move the coordinator"
        );
        assert!(
            live_context_has_local_rules(&fixture),
            "preserveMode guards the mode only; the rule the user granted still lands"
        );
        assert_eq!(
            persisted_local_rules(&fixture),
            Some(json!(["Bash(cargo build)"])),
            "CC `:263` `persistPermissionUpdates(permissionUpdates)` still runs"
        );

        // The other side of the same discriminator: the leader answering its
        // own row IS the user-initiated change CC refuses to override.
        let (leader_tx, _leader_waiter) = async_channel::bounded(1);
        let own_row = ToolUseConfirm::new(racing_bash_request("toolu_leader")).with_responder(
            crate::types::permissions::PermissionPromptResponder::new(leader_tx),
        );
        assert_eq!(
            own_row.source,
            crate::types::permissions::PermissionRowSource::Interactive
        );
        let PermissionAnswerClaim::Won(response) =
            claim_permission_answer(&own_row, answer_with_a_mode_switch(), &fixture.store)
        else {
            panic!("the only racer must win the claim");
        };
        let effective_request = response.apply_to_request(own_row.request.clone());
        apply_permission_answer_to_leader_context(
            &own_row,
            &fixture.store,
            &effective_request,
            &response,
        );
        assert_eq!(
            fixture.store.tool_permission_context().mode,
            crate::types::permissions::PermissionMode::AcceptEdits,
            "`PermissionContext.ts:143-145` passes no options, so the incoming mode wins"
        );

        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    /// Maps to: CC `inProcessRunner.ts:209-217` — the abort listener sets
    /// `decisionMade = true` and filters the row out, both synchronously, on the
    /// same event loop the dialog's key handler runs on. An answer handled after
    /// an abort therefore returns from `onAllow`'s `if (!claim()) return`
    /// (`:256`) having persisted nothing.
    ///
    /// The setter here is the PRODUCTION one — `SetToolUseConfirmQueueFn` over
    /// the REPL's updater channel (`screens/repl.rs`, `use_const` at the
    /// `tool_use_confirm_channel` binding) — not `test_leader_queue`'s
    /// synchronous applier, because the whole bug is that production's applier
    /// runs LATER. A test built on the synchronous applier exercises a shape
    /// production does not have and cannot fail.
    ///
    /// OLD SHAPE: the answer path read `permission_queue` as the last drain left
    /// it and claimed the row it found there, so inside the window between the
    /// abort's `try_send` and the REPL's next drain the answer took the claim,
    /// wrote `Bash(cargo build)` into `settings.local.json`, and only then
    /// discovered the teammate was gone. All four assertions in the first leg
    /// failed: the answer was returned as won, the queue kept the row, and both
    /// the file and the live context carried the rule. It fails, it does not
    /// hang — nothing in this path waits on a channel.
    #[test]
    fn an_abort_published_before_the_answer_takes_the_claim_first() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let fixture = racing_permission_fixture("abort-window");

        let (updater_tx, updater_rx) = async_channel::unbounded::<
            crate::utils::swarm::leader_permission_bridge::ToolUseConfirmQueueUpdater,
        >();
        let setter = crate::utils::swarm::leader_permission_bridge::SetToolUseConfirmQueueFn::new(
            move |updater| {
                let _ = updater_tx.try_send(updater);
            },
        );

        let answer_for = |tool_use_id: &str| {
            let mut response = always_allow_with_a_persisted_rule();
            response.tool_use_id = Some(tool_use_id.to_string());
            response
        };
        let asking_row = |tool_use_id: &str| {
            let (response_tx, waiter) = async_channel::bounded(1);
            let row = ToolUseConfirm::new(racing_bash_request(tool_use_id)).with_responder(
                crate::types::permissions::PermissionPromptResponder::new(response_tx),
            );
            (row, waiter)
        };

        let (row, _waiter) = asking_row("toolu_abort");
        setter.push_to_queue(row);
        // One pass of the REPL's drain future: the row is now on screen.
        let mut queue = settle_pending_permission_queue_updaters(&updater_rx, Vec::new());
        assert_eq!(queue.len(), 1);

        // The teammate's turn aborts. CC would have marked the decision made
        // here; this port publishes claim-and-withdraw as an updater
        // (`teammate_leader_dialog_sink` holds the id, not the responder), and
        // the REPL has not applied it yet — that gap is the window.
        setter.claim_and_remove_from_queue("toolu_abort");
        assert_eq!(
            queue.len(),
            1,
            "the dialog is still rendering the row: the user can still answer it"
        );

        // The user answers inside the window.
        let answered = claim_permission_answer_from_queue(
            &updater_rx,
            &mut queue,
            answer_for("toolu_abort"),
            &fixture.store,
        );
        assert!(
            answered.is_none(),
            "the abort took the claim first, so the answer is not a decision"
        );
        assert!(queue.is_empty(), "and the abort's withdrawal still applies");
        assert_eq!(
            persisted_local_rules(&fixture),
            None,
            "a lost answer must not reach settings.local.json"
        );
        assert!(
            !live_context_has_local_rules(&fixture),
            "a lost answer must not move the live permission context"
        );

        // The ordinary path, to prove the settle is not simply swallowing
        // answers: with nothing published against it, the answer wins.
        let (row, _waiter) = asking_row("toolu_answered");
        setter.push_to_queue(row);
        let mut queue = settle_pending_permission_queue_updaters(&updater_rx, queue);
        assert_eq!(queue.len(), 1);
        let answered = claim_permission_answer_from_queue(
            &updater_rx,
            &mut queue,
            answer_for("toolu_answered"),
            &fixture.store,
        );
        assert!(
            answered.is_some(),
            "an unopposed answer still wins its claim"
        );
        assert!(queue.is_empty());
        assert_eq!(
            persisted_local_rules(&fixture),
            Some(json!(["Bash(cargo build)"]))
        );

        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    /// Maps to: CC `useCanUseTool.tsx:70,307-324` — the entry the REPL queues
    /// for its OWN query closes over that query's `resolve`, which is what lets
    /// `interactiveHandler.ts:204-231` `recheckPermission` settle it.
    ///
    /// OLD SHAPE: rows raised by `QueryEvent::PermissionRequest` carried no
    /// resolver at all, so the sweep skipped them (`recheck_permission`'s first
    /// guard) and the answer was addressed at whatever `active_query` named at
    /// keypress time. The assertions below both failed: no responder, and no
    /// command delivered.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_repls_own_query_row_carries_a_resolver_the_sweep_can_settle() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let fixture = racing_permission_fixture("repl-owned");
        let (command_tx, command_rx) = async_channel::unbounded::<QueryCommand>();

        // What `QueryEvent::PermissionRequest` builds: the row plus a resolver
        // bound to the command channel of the handle that raised it.
        let request = racing_bash_request("toolu_repl_owned");
        let tool_use_id = request.tool_use_id.clone();
        let row = tool_use_confirm_for_request(
            request,
            crate::types::permissions::PermissionPromptResponder::from_command_sink(
                move |response| {
                    command_tx
                        .try_send(QueryCommand::PermissionResponse {
                            tool_use_id: tool_use_id.clone(),
                            response,
                        })
                        .is_ok()
                },
            ),
            // CC `interactiveHandler.ts:97` for the REPL's own query: the
            // asking context is the leader's live one, so no override rides
            // the row.
            None,
        );
        assert!(row.responder.is_some());
        fixture.leader.setter.push_to_queue(row.clone());

        assert!(
            crate::hooks::tool_permission::handlers::interactive_handler::recheck_permission(
                &row,
                &fixture.store,
                Some(fixture.leader.setter.clone()),
            )
            .await
        );
        let QueryCommand::PermissionResponse {
            tool_use_id,
            response,
        } = command_rx.try_recv().expect("the sweep settles the row")
        else {
            panic!("recheck resolves with a permission response");
        };
        assert_eq!(tool_use_id, "toolu_repl_owned");
        assert_eq!(response.choice, PermissionPromptChoice::AllowOnce);
        assert!(
            !fixture
                .leader
                .queue
                .lock()
                .unwrap()
                .iter()
                .any(|entry| entry.tool_use_id() == "toolu_repl_owned")
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    /// Maps to: CC `components/permissions/PermissionRequest.tsx:196-231` — the
    /// dialog holds the ROW and answers it by method
    /// (`toolUseConfirm.onAllow(...)`), so an answer can never reach an entry
    /// the user was not shown. `toolUseConfirmQueue[0]` is CC's RENDER
    /// selection (`REPL.tsx:6035-6046`), never its answer address.
    ///
    /// The window this closes: the head can change between the render and the
    /// keypress. Since #179 a teammate abort withdraws its own row
    /// (`inProcessRunner.ts:214-216`), and since #218 the recheck sweep
    /// withdraws rows it resolves (`interactiveHandler.ts:227`).
    ///
    /// OLD SHAPE: `on_permission_response` did `queue.remove(0)` after only an
    /// `is_empty` guard, so the answer for the withdrawn row A landed on its
    /// successor B: B was claimed, its rules persisted, its promise resolved
    /// with a decision the user made about a different tool. The failure is an
    /// assertion failure, not a hang — B's waiter receives an answer it should
    /// never have got. Both `assert!(...is_none())` and the four
    /// "B is untouched" assertions below failed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_answer_for_a_withdrawn_row_is_discarded_rather_than_applied_to_its_successor() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let fixture = racing_permission_fixture("withdrawn-head");

        // The successor: a second row queued behind the one on screen.
        let (successor_tx, successor_rx) = async_channel::bounded(1);
        let successor = ToolUseConfirm::new(racing_bash_request("toolu_successor")).with_responder(
            crate::types::permissions::PermissionPromptResponder::new(successor_tx),
        );
        fixture.leader.setter.push_to_queue(successor.clone());
        assert_eq!(fixture.leader.queue.lock().unwrap().len(), 2);

        // The head is withdrawn AFTER the dialog rendered it — here by the
        // sweep, which claims and resolves it on the way out.
        assert!(
            crate::hooks::tool_permission::handlers::interactive_handler::recheck_permission(
                &fixture.row,
                &fixture.store,
                Some(fixture.leader.setter.clone()),
            )
            .await
        );
        let queue = fixture.leader.queue.lock().unwrap().clone();
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue[0].tool_use_id(),
            "toolu_successor",
            "the successor is now the head — what index addressing would answer"
        );

        // The keypress: the answer names the row the dialog rendered
        // (`components/permissions/permission_request.rs` stamps it).
        let answer = always_allow_with_a_persisted_rule()
            .with_tool_use_id(Some(fixture.row.tool_use_id().to_string()));
        assert!(
            permission_queue_index_for_answer(&queue, answer.tool_use_id.as_deref()).is_none(),
            "a row that is gone takes no answer"
        );

        assert!(
            !successor.responder.is_resolved(),
            "the successor must not be claimed by an answer meant for another row"
        );
        assert!(successor_rx.try_recv().is_err());
        assert_eq!(
            persisted_local_rules(&fixture),
            None,
            "a discarded answer must not reach settings.local.json"
        );
        assert!(!live_context_has_local_rules(&fixture));

        // The same answer, while its row IS still queued, lands on that row and
        // on no other — index-independently.
        let queue = vec![successor.clone(), fixture.row.clone()];
        assert_eq!(
            permission_queue_index_for_answer(&queue, answer.tool_use_id.as_deref()),
            Some(1)
        );
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    /// The `None` arm: an answer that names no row (the props-default dialog,
    /// and direct test callers) keeps the head-of-queue behaviour, and an empty
    /// queue takes nothing.
    #[test]
    fn an_unnamed_answer_still_addresses_the_head_and_an_empty_queue_takes_none() {
        let head = ToolUseConfirm::new(racing_bash_request("toolu_head"));
        let queue = vec![head, ToolUseConfirm::new(racing_bash_request("toolu_tail"))];
        assert_eq!(permission_queue_index_for_answer(&queue, None), Some(0));
        assert_eq!(permission_queue_index_for_answer(&[], None), None);
        assert_eq!(
            permission_queue_index_for_answer(&[], Some("toolu_head")),
            None
        );
    }

    #[tokio::test]
    async fn worker_forwarded_ask_uses_leader_response_and_never_enters_local_queue() {
        // Maps to: CC REPL.tsx:2937-2975 — the swarm-worker branch resolves
        // via the leader mailbox and must not touch the local dialog queue.
        let (local_tx, local_rx) = async_channel::unbounded::<LocalSandboxPermissionAsk>();
        let (leader_tx, leader_rx) = async_channel::bounded(1);
        leader_tx.try_send(true).unwrap();
        let allowed = repl_sandbox_ask_flow(
            &local_tx,
            crate::utils::sandbox::sandbox_adapter::NetworkHostPattern::new("api.example.com"),
            Some(leader_rx),
        )
        .await;
        assert!(allowed);
        assert!(
            local_rx.try_recv().is_err(),
            "worker-forwarded ask must never enter the local queue"
        );
    }

    #[tokio::test]
    async fn local_ask_resolves_with_dialog_answer() {
        // Maps to: CC REPL.tsx:2979-2994 — the queued entry's resolvePromise
        // resolves the callback's promise.
        let (local_tx, local_rx) = async_channel::unbounded::<LocalSandboxPermissionAsk>();
        let flow = tokio::spawn({
            let local_tx = local_tx.clone();
            async move {
                repl_sandbox_ask_flow(
                    &local_tx,
                    crate::utils::sandbox::sandbox_adapter::NetworkHostPattern::new(
                        "api.example.com",
                    ),
                    None,
                )
                .await
            }
        });
        let ask = local_rx.recv().await.unwrap();
        assert_eq!(ask.host_pattern.host, "api.example.com");
        (ask.resolve)(true);
        // CC resolveOnce guard equivalent: a second resolution is inert.
        (ask.resolve)(false);
        assert!(flow.await.unwrap());
    }

    #[tokio::test]
    async fn closed_repl_carrier_denies_asker() {
        // Guard-drop path 1: the REPL unmounted before the ask was queued —
        // the channel receiver is gone, the producer is denied.
        let (local_tx, local_rx) = async_channel::unbounded::<LocalSandboxPermissionAsk>();
        drop(local_rx);
        assert!(
            !repl_sandbox_ask_flow(
                &local_tx,
                crate::utils::sandbox::sandbox_adapter::NetworkHostPattern::new("api.example.com"),
                None,
            )
            .await
        );
    }

    #[tokio::test]
    async fn dropped_queue_entry_denies_parked_asker() {
        // Guard-drop path 2: the REPL unmounted with the ask queued — the
        // State-owned entry (and its resolver sender) drops, so the parked
        // producer resolves false instead of hanging (pre-slice leak/hang).
        let (local_tx, local_rx) = async_channel::unbounded::<LocalSandboxPermissionAsk>();
        let flow = tokio::spawn({
            let local_tx = local_tx.clone();
            async move {
                repl_sandbox_ask_flow(
                    &local_tx,
                    crate::utils::sandbox::sandbox_adapter::NetworkHostPattern::new(
                        "api.example.com",
                    ),
                    None,
                )
                .await
            }
        });
        let ask = local_rx.recv().await.unwrap();
        drop(ask);
        drop(local_rx);
        assert!(!flow.await.unwrap());
    }

    #[test]
    fn repl_on_query_command_permissions_match_official_set_and_next_turn_reset() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let allowed = apply_repl_command_allowed_tools(
            &store,
            &["Read(/repo/**)".to_string(), "Grep".to_string()],
        );
        assert_eq!(
            allowed.always_allow_rules[&crate::types::permissions::PermissionRuleSource::Command],
            vec![
                crate::types::permissions::PermissionRuleValue::new(
                    "Read",
                    Some("/repo/**".to_string()),
                ),
                crate::types::permissions::PermissionRuleValue::new("Grep", None),
            ]
        );
        assert_eq!(store.tool_permission_context(), allowed);

        let reset = apply_repl_command_allowed_tools(&store, &[]);
        assert!(
            reset.always_allow_rules[&crate::types::permissions::PermissionRuleSource::Command]
                .is_empty()
        );
        assert_eq!(store.tool_permission_context(), reset);
    }

    /// The converter is a carrier hop, not a filter. CC keeps every forwarded
    /// message in `progressMessages` — `extractLastToolInfo` counts tool_result
    /// rows (`UI.tsx:1053-1060`) and `calculateAgentStats` counts user rows
    /// carrying one (`:795-804`), both of which need the unpreserved ones. The
    /// raw-preservation policy that used to drop them here now lives where CC
    /// has it: the `VerboseAgentTranscript` row filter (`UI.tsx:293-296`).
    #[test]
    fn subagent_progress_converter_keeps_unpreserved_results_and_rewraps_identically() {
        fn nested_result(
            uuid: &str,
            tool_use_id: &str,
            content: &str,
            tool_use_result: Option<serde_json::Value>,
        ) -> crate::types::message::Message {
            let row = crate::types::message::RenderableMessage::user_tool_result(
                uuid,
                tool_use_id,
                content,
                false,
            )
            .with_tool_use_result(tool_use_result);
            match row.kind {
                crate::types::message::RenderableMessageKind::User { message } => {
                    crate::types::message::Message::User(message)
                }
                _ => unreachable!("user_tool_result builds a user row"),
            }
        }

        let rewrapped = subagent_progress_render_message(nested_result(
            "user-uuid",
            "toolu_nested_grep",
            "Found 1 file\na.rs",
            None,
        ))
        .expect("carrier hop keeps the row");
        assert_eq!(rewrapped.uuid, "user-uuid");
        assert!(matches!(
            &rewrapped.kind,
            crate::types::message::RenderableMessageKind::User { message }
                if matches!(
                    &message.content[0],
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_id.0 == "toolu_nested_grep"
                            && result.tool_use_result.is_none()
                )
        ));

        // A preserved raw rides through untouched — the by-tool-name renderer
        // downstream is the only consumer of its shape.
        let rewrapped = subagent_progress_render_message(nested_result(
            "user-uuid-2",
            "toolu_nested_read",
            "Read 2 lines",
            Some(serde_json::json!({
                "type": "text",
                "file": {
                    "filePath": "src/main.rs",
                    "content": "a\nb",
                    "numLines": 2,
                    "startLine": 1,
                    "totalLines": 2
                }
            })),
        ))
        .expect("carrier hop keeps the row");
        assert!(matches!(
            &rewrapped.kind,
            crate::types::message::RenderableMessageKind::User { message }
                if matches!(
                    &message.content[0],
                    crate::types::message::UserContent::ToolResult(result)
                        if result.tool_use_result.as_ref().is_some_and(|raw| {
                            raw["file"]["numLines"] == serde_json::json!(2)
                        })
                )
        ));
    }

    fn progress_history_entry(
        parent_tool_use_id: &str,
        data: crate::types::message::ToolUseProgressMessage,
    ) -> crate::types::message::ProgressMessage {
        crate::types::message::ProgressMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            tool_use_id: parent_tool_use_id.to_string(),
            parent_tool_use_id: parent_tool_use_id.to_string(),
            data,
        }
    }

    fn bash_tick(output: &str) -> crate::types::message::ToolUseProgressMessage {
        crate::types::message::ToolUseProgressMessage::BashProgress {
            output: output.to_string(),
            full_output: output.to_string(),
            elapsed_time_seconds: 1,
            total_lines: 1,
            total_bytes: None,
            task_id: None,
            timeout_ms: None,
        }
    }

    #[test]
    fn ephemeral_progress_replaces_last_matching_tick_like_official_repl() {
        // CC REPL.tsx:3482-3494 — consecutive bash ticks for the same tool
        // call replace the last message in place instead of appending.
        let mut entries = Vec::new();
        push_or_replace_progress_entry(
            &mut entries,
            progress_history_entry("toolu_bash", bash_tick("one")),
        );
        push_or_replace_progress_entry(
            &mut entries,
            progress_history_entry("toolu_bash", bash_tick("two")),
        );
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            HistoryEntry::Message(Message::Progress(progress))
                if matches!(
                    &progress.data,
                    crate::types::message::ToolUseProgressMessage::BashProgress { output, .. }
                        if output == "two"
                )
        ));

        // Different tool call: appended, not replaced.
        push_or_replace_progress_entry(
            &mut entries,
            progress_history_entry("toolu_other", bash_tick("three")),
        );
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn non_ephemeral_progress_always_appends_like_official_repl() {
        // CC REPL.tsx:3478-3481 — agent/hook/skill progress carries distinct
        // state per message and must accumulate; the consumer picks the newest
        // from the lookups (BashTool/UI.tsx:140 `.at(-1)`).
        let subagent = || crate::types::message::ToolUseProgressMessage::AgentProgress {
            message: Box::new(crate::types::message::RenderableMessage::assistant_block(
                "nested-assistant",
                crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_nested_read".to_string()),
                        name: "Read".to_string(),
                        input: serde_json::json!({ "file_path": "src/main.rs" }),
                    },
                ),
            )),
            prompt: String::new(),
            agent_id: "agent-1".to_string(),
        };
        let mut entries = Vec::new();
        push_or_replace_progress_entry(
            &mut entries,
            progress_history_entry("toolu_agent", subagent()),
        );
        push_or_replace_progress_entry(
            &mut entries,
            progress_history_entry("toolu_agent", subagent()),
        );
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn initial_tools_take_precedence_and_keep_builtin_prefix_like_official_merge() {
        let permission = crate::tool::ToolPermissionContext::default();
        let mut override_tool = crate::tools::get_tools(&permission)
            .into_iter()
            .next()
            .expect("builtin tool registry");
        override_tool.description = "launch override".to_string();
        let override_name = override_tool.name.clone();
        let injected_mcp = crate::types::tools::Tool {
            name: "mcp__launch__tool".to_string(),
            description: "startup MCP tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            is_mcp: true,
            ..Default::default()
        };

        // `ReplPromptTools` is not reachable from here by design (see the
        // `repl_prompt_tools` module doc), so the memo is exercised the only way
        // production can reach it: through the installer, on a context.
        let store_definitions = std::sync::Arc::new(
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default(),
        );
        let mut context = ToolUseContext::with_permission_context(permission);
        install_repl_prompt_tools(
            &mut context,
            &store_definitions,
            &[override_tool, injected_mcp],
            None,
        );
        // CC `REPL.tsx:1205-1211`: no main-thread agent → `allowedAgentTypes`
        // is `undefined`, and `:3208` hands back the store object by identity.
        assert_eq!(context.agent_definitions.allowed_agent_types, None);
        assert!(std::sync::Arc::ptr_eq(
            &context.agent_definitions,
            &store_definitions
        ));
        let merged = context.tools;
        let matching = merged
            .iter()
            .filter(|tool| tool.name == override_name)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].description, "launch override");
        let first_mcp = merged.iter().position(|tool| tool.is_mcp).unwrap();
        assert!(merged[..first_mcp].iter().all(|tool| !tool.is_mcp));
        assert!(merged[first_mcp..].iter().all(|tool| tool.is_mcp));
    }

    #[derive(Clone)]
    struct ForkRestoreCommitProbe {
        result: crate::utils::session_restore::ProcessedResume,
        store: crate::state::store::AppStore,
        committed: Arc<std::sync::atomic::AtomicBool>,
    }

    #[component]
    fn ForkRestoreCommitHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let probe = hooks.use_context::<ForkRestoreCommitProbe>().clone();
        let mut history = hooks.use_state(|| Arc::new(Vec::new()));
        let mut pending = hooks.use_state(Vec::new);
        let mut replacement =
            hooks.use_state(
                || crate::utils::tool_result_storage::ContentReplacementState {
                    seen_ids: std::collections::HashSet::from(["frozen-tool-id".into()]),
                    replacements: std::collections::HashMap::from([(
                        "frozen-tool-id".into(),
                        "live frozen preview".into(),
                    )]),
                },
            );
        let mut stores = hooks.use_state(ResumeRestoreStores::default);
        let mut memory = hooks.use_state(std::collections::HashSet::new);
        let mut agent = hooks.use_state(|| None);
        let live_history = probe.store.get().file_history.clone();
        hooks.use_effect(
            move || {
                apply_resume_restore_result(
                    &mut history,
                    &mut pending,
                    &mut replacement,
                    &mut stores,
                    &mut memory,
                    &mut agent,
                    &probe.store,
                    probe.result,
                );
                assert!(replacement.read().seen_ids.contains("frozen-tool-id"));
                assert_eq!(
                    replacement
                        .read()
                        .replacements
                        .get("frozen-tool-id")
                        .map(String::as_str),
                    Some("live frozen preview")
                );
                assert!(!replacement.read().seen_ids.contains("recorded-tool-id"));
                assert!(Arc::ptr_eq(&probe.store.get().file_history, &live_history));
                probe
                    .committed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            },
            (),
        );
        element!(Text(content: "fork commit"))
    }

    #[test]
    fn fork_apply_matches_official_live_replacement_and_file_history_preservation() {
        // CC REPL.tsx:2548-2558 skips replacement reconstruction for Fork;
        // sessionRestore.ts:104-109 keeps fileHistory when log omits snapshots.
        let _env = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let target = resume::ResumeTarget {
            session_id: Uuid::new_v4().to_string(),
            project_path: None,
            entries: vec![
                json!({"type": "user", "message": {"role": "user", "content": "forked"}}),
            ],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: resume::ResumeMetadata {
                content_replacements: vec![
                    json!({"kind": "tool-result", "toolUseId": "recorded-tool-id", "replacement": "recorded preview"}),
                ],
                ..Default::default()
            },
            entrypoint: Some(ResumeEntrypoint::Fork),
        };
        let result = crate::utils::session_restore::ProcessedResume::from_load_result(
            crate::utils::session_restore::ResumeLoadResult::try_from(&target).unwrap(),
            None,
            Arc::new(Default::default()),
        );
        let mut app_state = crate::state::app_state_store::AppState::default();
        app_state.file_history = Arc::new(crate::utils::file_history::FileHistoryState {
            snapshot_sequence: 42,
            ..Default::default()
        });
        let probe = ForkRestoreCommitProbe {
            result,
            store: crate::state::store::AppStore::new(app_state, None),
            committed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        futures::executor::block_on(async {
            let mut app = element! {
                ContextProvider(value: Context::owned(probe.clone())) { ForkRestoreCommitHarness }
            };
            let mut frames = Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::default()));
            for _ in 0..5 {
                let next = crate::utils::race(frames.next(), async {
                    futures_timer::Delay::new(Duration::from_secs(1)).await;
                    None
                })
                .await;
                if next.is_none() || probe.committed.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
            }
        });
        assert!(probe.committed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn branch_command_matches_official_hot_resume_and_system_output() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = BranchFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());
        let _project = crate::utils::env_utils::PinnedProjectDir::at(temp.path());
        let _projects = crate::utils::session_storage::set_test_projects_dir_override(
            temp.path().join("projects"),
        );
        let _no_hooks = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        struct RestoreSession(String, Option<PathBuf>);
        impl Drop for RestoreSession {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(self.0.clone(), self.1.clone());
                crate::utils::session_storage::clear_session_metadata();
                crate::utils::session_storage::reset_session_file_pointer();
            }
        }
        let _session = RestoreSession(
            crate::bootstrap::state::get_session_id(),
            crate::bootstrap::state::get_session_project_dir(),
        );
        let source_id = Uuid::new_v4().to_string();
        crate::bootstrap::state::switch_session(source_id.clone(), None);
        crate::utils::session_storage::reset_session_file_pointer();
        let source_file = write_resume_fixture(
            &temp.path().display().to_string(),
            &source_id,
            "branch original prompt",
            "branch original answer",
        );
        // CC recordTranscript persists ISO timestamps. This shared minimal
        // fixture omits them; supply valid source times here so loadFullLog's
        // findLatestMessage can select its user/assistant leaf rather than
        // exercising the malformed-log fallback.
        let mut source_rows =
            crate::utils::session_storage::load_session_raw_from_path(&source_file);
        let source_time = chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00.000Z").unwrap();
        for (index, entry) in source_rows.iter_mut().enumerate() {
            entry["timestamp"] = serde_json::Value::String(
                (source_time + chrono::Duration::milliseconds(index as i64))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            );
        }
        let source_jsonl = source_rows
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&source_file, format!("{source_jsonl}\n")).unwrap();
        let before = std::fs::read(&source_file).unwrap();
        let (query_events, query_event_rx) = async_channel::unbounded();
        let (query_commands, _query_command_rx) = async_channel::unbounded();
        let probe = ReplActiveQueryProbe {
            start_active: false,
            started_queries: Arc::new(Mutex::new(Vec::new())),
            handle: QueryHandle {
                id: "unexpected-branch-query".to_string(),
                events: Arc::new(query_event_rx),
                commands: Arc::new(query_commands),
                abort_controller: crate::tool::AbortController::default(),
                resume: None,
            },
            snapshot: Arc::new(Mutex::new(None)),
        };
        let text = futures::executor::block_on(async {
            let mut app = element! {
                ContextProvider(value: Context::owned(probe.clone())) { ReplHarness }
            };
            let mut frames = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(text_input_events(
                        "/branch copied",
                    )))
                    .with_size(180, 40),
                ),
            );
            let mut text = String::new();
            for _ in 0..150 {
                let next = crate::utils::race(frames.next(), async {
                    futures_timer::Delay::new(Duration::from_secs(3)).await;
                    None
                })
                .await;
                let Some(frame) = next else { break };
                text = canvas_lines(&frame).join("\n");
                if text.contains("You are now in the branch.") {
                    break;
                }
            }
            text
        });
        assert!(
            text.contains("You are now in the branch."),
            "canvas=\n{text}"
        );
        assert!(text.contains("branch original answer"), "canvas=\n{text}");
        assert_ne!(crate::bootstrap::state::get_session_id(), source_id);
        assert_eq!(std::fs::read(&source_file).unwrap(), before);
        assert!(probe.started_queries.lock().unwrap().is_empty());
        let snapshot = probe.snapshot.lock().unwrap();
        let snapshot = snapshot.as_ref().unwrap();
        let system_rows = snapshot
            .rows
            .iter()
            .filter(|row| crate::utils::messages::is_system_local_command_message(row))
            .collect::<Vec<_>>();
        assert_eq!(
            system_rows.len(),
            2,
            "onDone(system) emits command input + stdout"
        );
        let typed_receipts = snapshot
            .model_messages
            .iter()
            .filter_map(|message| match message {
                Message::System(SystemMessage::LocalCommand { base, content }) => {
                    Some((base.uuid.clone(), content.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            typed_receipts.len(),
            2,
            "system command receipts belong to persisted typed history"
        );
        assert_eq!(
            typed_receipts
                .iter()
                .map(|(uuid, _)| uuid.as_str())
                .collect::<Vec<_>>(),
            system_rows
                .iter()
                .map(|row| row.uuid.as_str())
                .collect::<Vec<_>>()
        );
        // Source recordTranscript flush + UUID cold loader: the same shared
        // typed history must retain both local_command rows after restart.
        // The real hot rows above may share a millisecond. CC findLatestMessage
        // uses strict Date.parse >, so ties intentionally choose the first row.
        // Give only this persistence clone distinct clock ticks to exercise
        // recovery of both linked receipts deterministically; keep UUID/content
        // and the live production output untouched.
        let mut persistence_messages = snapshot.model_messages.clone();
        let first_receipt_time = persistence_messages
            .iter()
            .find_map(|message| match message {
                Message::System(SystemMessage::LocalCommand { base, .. }) => Some(base.timestamp),
                _ => None,
            })
            .unwrap();
        let mut receipt_index = 0;
        for message in &mut persistence_messages {
            if let Message::System(SystemMessage::LocalCommand { base, .. }) = message {
                base.timestamp = first_receipt_time + chrono::Duration::milliseconds(receipt_index);
                receipt_index += 1;
            }
        }
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::utils::session_storage::record_typed_messages_with_team(
            &persistence_messages,
            None,
            None,
        )
        .unwrap();
        crate::utils::process_runtime::block_on_from_sync(async {
            crate::utils::session_storage::flush_session_storage().await
        })
        .unwrap()
        .unwrap();
        let fork_id = crate::bootstrap::state::get_session_id();
        let fork_path = crate::utils::session_storage::get_session_file_path(
            &temp.path().display().to_string(),
            &fork_id,
        );
        // CC insertMessageChain:1003-1010 / appendEntry:1138-1142: fork has
        // reset the active file pointer. Dedup leaves only these new system
        // rows, so flush drains writes but does not materialize pending entries.
        assert!(
            crate::utils::session_storage::load_session_raw_from_path(&fork_path)
                .iter()
                .all(|entry| !(entry["type"] == "system" && entry["subtype"] == "local_command"))
        );
        // The next real user message materializes the fork and its pending
        // receipts. Exercise the canonical writer without sending an API call.
        let mut next_user = crate::utils::messages::create_user_message(
            "Continue working in this branched conversation.".to_string(),
        );
        next_user.timestamp = first_receipt_time + chrono::Duration::milliseconds(2);
        let next_user_id = next_user.uuid.clone();
        persistence_messages.push(Message::User(next_user));
        crate::utils::session_storage::record_typed_messages_with_team(
            &persistence_messages,
            None,
            None,
        )
        .unwrap();
        crate::utils::process_runtime::block_on_from_sync(async {
            crate::utils::session_storage::flush_session_storage().await
        })
        .unwrap()
        .unwrap();
        let stored = crate::utils::session_storage::load_session_raw_from_path(&fork_path);
        let stored_receipts = stored
            .iter()
            .filter(|entry| entry["type"] == "system" && entry["subtype"] == "local_command")
            .map(|entry| {
                (
                    entry["uuid"].as_str().unwrap().to_string(),
                    entry["content"].as_str().unwrap().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(stored_receipts, typed_receipts);
        let cold =
            resume::load_for_cli_resume(&temp.path().display().to_string(), &fork_id).unwrap();
        let cold_messages =
            crate::utils::conversation_recovery::messages_from_entries(&cold.entries);
        let cold_receipts = cold_messages
            .iter()
            .filter_map(|message| match message {
                Message::System(SystemMessage::LocalCommand { base, content }) => {
                    Some((base.uuid.clone(), content.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(cold_receipts, typed_receipts);
        assert!(cold_messages.iter().any(|message| {
            matches!(message, Message::User(user) if user.uuid == next_user_id)
        }));
        // CC loadFullLog:2988-2999 intentionally uses the last user/assistant
        // leaf. With a new user AFTER the receipts, they are ancestors of that
        // leaf and must survive picker/continue and indexed /resume too.
        let picker = resume::load_for_picker(&temp.path().display().to_string(), &fork_id).unwrap();
        let slash = resume::load_for_arg(&temp.path().display().to_string(), &fork_id).unwrap();
        let continued = resume::load_for_continue(&temp.path().display().to_string()).unwrap();
        assert_eq!(continued.session_id, fork_id);
        for restored in [&picker, &slash, &continued] {
            let receipts = restored
                .entries
                .iter()
                .filter(|entry| entry["type"] == "system" && entry["subtype"] == "local_command")
                .map(|entry| {
                    (
                        entry["uuid"].as_str().unwrap().to_string(),
                        entry["content"].as_str().unwrap().to_string(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(receipts, typed_receipts);
            assert!(
                restored
                    .entries
                    .iter()
                    .any(|entry| entry["uuid"] == next_user_id)
            );
        }
        let cold_api = crate::utils::messages::normalize_messages_for_api(cold_messages);
        assert_eq!(std::fs::read(&source_file).unwrap(), before);
        let hot_api =
            crate::utils::messages::normalize_messages_for_api(snapshot.model_messages.clone());
        // CC messages.ts:2059-2093 preserves local_command as API User text.
        // The old "not sent to model" comment in processSlashCommand is not
        // authoritative over the actual normalization branch.
        for api in [&hot_api, &cold_api] {
            assert!(
                api.iter()
                    .all(|message| !matches!(message, Message::System(_)))
            );
            let command_users = api
                .iter()
                .filter_map(|message| match message {
                    Message::User(user) => {
                        let text = user
                            .content
                            .iter()
                            .filter_map(|block| match block {
                                UserContent::Text(text) | UserContent::MetaText(text) => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                            .collect::<String>();
                        text.contains("<command-name>/branch</command-name>")
                            .then_some((user, text))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                command_users.len(),
                1,
                "two local_command rows merge into one API user after the assistant"
            );
            let (user, text) = &command_users[0];
            assert_eq!(user.uuid, typed_receipts[0].0);
            assert_eq!(
                text.matches("<command-name>/branch</command-name>").count(),
                1
            );
            assert_eq!(text.matches("You are now in the branch.").count(), 1);
            assert!(text.contains("<local-command-stdout>"));
        }
        drop(query_events);
    }

    #[test]
    fn fork_resume_matches_official_worktree_metadata_and_live_file_history() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = BranchFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());
        let _project = crate::utils::env_utils::PinnedProjectDir::at(temp.path());
        let _no_hooks = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        struct RestoreSession {
            id: String,
            project: Option<PathBuf>,
            worktree: Option<crate::utils::worktree::WorktreeSession>,
            cwd: PathBuf,
        }
        impl Drop for RestoreSession {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(self.id.clone(), self.project.clone());
                crate::utils::worktree::restore_worktree_session(self.worktree.clone());
                let _ = std::env::set_current_dir(&self.cwd);
                crate::utils::session_storage::clear_session_metadata();
                crate::utils::session_storage::reset_session_file_pointer();
            }
        }
        let guard = RestoreSession {
            id: crate::bootstrap::state::get_session_id(),
            project: crate::bootstrap::state::get_session_project_dir(),
            worktree: crate::utils::worktree::get_current_worktree_session(),
            cwd: std::env::current_dir().unwrap(),
        };
        let worktree = crate::utils::worktree::WorktreeSession {
            original_cwd: temp.path().display().to_string(),
            worktree_path: guard.cwd.display().to_string(),
            worktree_name: "preserved-branch-worktree".into(),
            session_id: guard.id.clone(),
            creation_duration_ms: Some(123),
            used_sparse_paths: Some(true),
            ..Default::default()
        };
        crate::utils::worktree::restore_worktree_session(Some(worktree.clone()));
        crate::utils::session_storage::set_current_session_metadata_for_test(
            None,
            None,
            Some("previous-name"),
            None,
        );
        let target_id = Uuid::new_v4().to_string();
        let target_path = temp.path().join("forks").join(format!("{target_id}.jsonl"));
        let target = resume::ResumeTarget {
            session_id: target_id.clone(),
            project_path: None,
            entries: vec![json!({
                "type": "user", "uuid": Uuid::new_v4().to_string(),
                "message": {"role": "user", "content": "preserve worktree"}
            })],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: resume::ResumeMetadata {
                custom_title: Some("Preserved (Branch)".into()),
                full_path: Some(target_path.display().to_string()),
                ..Default::default()
            },
            entrypoint: Some(ResumeEntrypoint::Fork),
        };
        let processed = resume(target, None, Arc::new(Default::default()), None, None).unwrap();
        assert_eq!(crate::bootstrap::state::get_session_id(), target_id);
        assert_eq!(
            crate::bootstrap::state::get_session_project_dir(),
            target_path.parent().map(PathBuf::from)
        );
        assert_eq!(std::env::current_dir().unwrap(), guard.cwd);
        assert_eq!(
            crate::utils::worktree::get_current_worktree_session(),
            Some(worktree)
        );
        let metadata = crate::utils::session_storage::get_current_session_metadata();
        assert_eq!(metadata.custom_title.as_deref(), Some("Preserved (Branch)"));
        assert!(
            metadata.agent_name.is_none(),
            "clear metadata before restore prevents old-name leakage"
        );
        let saved_worktree = metadata.worktree_session.unwrap();
        assert!(saved_worktree.get("creationDurationMs").is_none());
        assert!(saved_worktree.get("usedSparsePaths").is_none());
        assert_eq!(saved_worktree["worktreeName"], "preserved-branch-worktree");
        assert!(
            !target_path.exists(),
            "hot resume must not adopt/materialize the pre-created fork file"
        );
        let mut state = crate::state::app_state_store::AppState::default();
        let live_history = Arc::new(crate::utils::file_history::FileHistoryState {
            snapshot_sequence: 42,
            tracked_files: std::collections::BTreeSet::from(["kept.txt".to_string()]),
            ..Default::default()
        });
        state.file_history = live_history.clone();
        processed.apply_to_app_state(&mut state);
        assert!(Arc::ptr_eq(&state.file_history, &live_history));
    }

    #[test]
    fn fork_resume_matches_official_session_end_then_explicit_target_start_identity() {
        let _env = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = BranchFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());
        let _project = crate::utils::env_utils::PinnedProjectDir::at(temp.path());
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        // Moving config/cwd away from just's trusted fixture removes its
        // persisted trust grant. Use the existing accepted-workspace fixture
        // so this test reaches hook execution rather than the trust skip.
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let old_id = crate::bootstrap::state::get_session_id();
        let old_project = crate::bootstrap::state::get_session_project_dir();
        let target_id = Uuid::new_v4().to_string();
        let marker = temp.path().join("hook-inputs.jsonl");
        let path = marker.display().to_string().replace('\'', "'\\''");
        let command = format!("cat >> '{path}'; printf '\\n' >> '{path}'");
        let settings = crate::utils::settings::SettingsJson {
            hooks: Some(json!({
                "SessionEnd": [{"matcher": "*", "hooks": [{"type": "command", "command": command, "timeout": 5}]}],
                "SessionStart": [{"matcher": "resume", "hooks": [{"type": "command", "command": command, "timeout": 5}]}]
            })),
            ..Default::default()
        };
        crate::utils::hooks::hooks_config_snapshot::capture_hooks_config_snapshot(
            &settings, None, false,
        );
        let target = resume::ResumeTarget {
            session_id: target_id.clone(),
            project_path: None,
            entries: vec![
                json!({"type": "user", "uuid": Uuid::new_v4().to_string(), "message": {"role": "user", "content": "fork lifecycle"}}),
            ],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: resume::ResumeMetadata::default(),
            entrypoint: Some(ResumeEntrypoint::Fork),
        };
        assert!(!crate::services::hooks::should_skip_hook_execution(
            crate::services::hooks::HookEvent::SessionEnd,
            "resume",
        ));
        let result = resume(target, None, Arc::new(Default::default()), None, None);
        crate::bootstrap::state::switch_session(old_id.clone(), old_project);
        crate::utils::session_storage::clear_session_metadata();
        crate::utils::session_storage::reset_session_file_pointer();
        result.unwrap();
        let text = std::fs::read_to_string(marker).unwrap();
        let inputs = serde_json::Deserializer::from_str(&text)
            .into_iter::<serde_json::Value>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0]["hook_event_name"], "SessionEnd");
        assert_eq!(inputs[0]["session_id"], old_id);
        assert_eq!(inputs[1]["hook_event_name"], "SessionStart");
        assert_eq!(inputs[1]["session_id"], target_id);
    }

    #[test]
    fn resume_runs_session_end_before_session_start_like_official_repl_callback() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _simple = crate::utils::env_utils::EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let marker_path = std::env::temp_dir().join(format!(
            "cometix-repl-resume-hooks-{}",
            uuid::Uuid::new_v4()
        ));
        let settings = crate::utils::settings::SettingsJson {
            hooks: Some(json!({
                "SessionEnd": [{
                    "matcher": "*",
                    "hooks": [{
                        "type": "command",
                        "command": append_resume_hook_marker_command(&marker_path, "end"),
                        "timeout": 5
                    }]
                }],
                "SessionStart": [{
                    "matcher": "resume",
                    "hooks": [{
                        "type": "command",
                        "command": append_resume_hook_marker_command(&marker_path, "start"),
                        "timeout": 5
                    }]
                }]
            })),
            ..crate::utils::settings::SettingsJson::default()
        };
        crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
        crate::utils::hooks::hooks_config_snapshot::capture_hooks_config_snapshot(
            &settings, None, false,
        );
        let target = resume::ResumeTarget {
            session_id: "repl-resume-session".to_string(),
            project_path: None,
            entries: vec![json!({
                "type": "user",
                "timestamp": "2026-07-12T00:00:00.000Z",
                "message": {"role": "user", "content": "resume hooks"}
            })],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: resume::ResumeMetadata::default(),
            entrypoint: Some(ResumeEntrypoint::SlashCommandPicker),
        };

        let processed = resume(
            target,
            None,
            Arc::new(crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default()),
            Some("reviewer".to_string()),
            Some("claude-sonnet".to_string()),
        )
        .expect("resume should succeed");
        assert_eq!(
            std::fs::read_to_string(&marker_path)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec!["end", "start"]
        );
        #[cfg(not(windows))]
        {
            assert!(processed.messages.iter().any(|message| matches!(
                message,
                Message::HookResult(hook)
                    if hook.attachment.get("content").and_then(serde_json::Value::as_str)
                        == Some("start hook message")
            )));
            assert!(processed.renderable_messages.iter().any(|message| matches!(
                &message.kind,
                RenderableMessageKind::System(SystemMessage::Informational { content: text, .. })
                    if text == "start hook message"
            )));
        }

        crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
        let _ = std::fs::remove_file(marker_path);
    }

    #[test]
    fn apply_repl_tool_use_context_options_matches_official_live_toggle() {
        let callbacks =
            crate::services::mcp::channel_permissions::ChannelPermissionCallbacks::default();
        let mut context = crate::tool::ToolUseContext::default();
        apply_repl_tool_use_context_options(
            &mut context,
            Some(&callbacks),
            &crate::tool::InteractivePermissionSink::default(),
            false,
            None,
            &crate::utils::thinking::ThinkingConfig::Adaptive,
            None,
            None,
        );
        assert_eq!(
            context.channel_permission_callbacks.as_ref(),
            Some(&callbacks)
        );
        assert!(context.main_loop_model.is_none());
        assert_eq!(
            context.thinking_config,
            Some(crate::utils::thinking::ThinkingConfig::Adaptive)
        );
        assert_eq!(context.fast_mode, Some(false));

        let mut context = crate::tool::ToolUseContext::default();
        apply_repl_tool_use_context_options(
            &mut context,
            None,
            &crate::tool::InteractivePermissionSink::default(),
            true,
            Some(false),
            &crate::utils::thinking::ThinkingConfig::Adaptive,
            Some("opus".to_string()),
            Some(crate::utils::effort::EffortValue::Named("high".to_string())),
        );
        assert_eq!(context.fast_mode, Some(true));
        // apply_repl_tool_use_context_options stores the caller-provided model
        // as-is; REPL callers pass `useMainLoopModel()`-resolved names.
        assert_eq!(context.main_loop_model.as_deref(), Some("opus"));
        assert_eq!(
            context.effort_value,
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );
        apply_repl_tool_use_context_options(
            &mut context,
            None,
            &crate::tool::InteractivePermissionSink::default(),
            true,
            Some(false),
            &crate::utils::thinking::ThinkingConfig::Adaptive,
            None,
            None,
        );
        assert!(
            context.effort_value.is_none(),
            "a reused context must clear the previous explicit effort"
        );
        // thinkingEnabled === false → disabled (CC REPL.tsx), ignores config.
        assert_eq!(
            context.thinking_config,
            Some(crate::utils::thinking::ThinkingConfig::Disabled)
        );
        assert!(context.channel_permission_callbacks.is_none());
    }

    #[test]
    fn process_input_context_matches_official_live_history_and_store_boundary() {
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "reviewer",
            "Reviews code",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::ProjectSettings,
        );
        let mut app_state = crate::state::app_state_store::AppState::default();
        app_state.agent_definitions = std::sync::Arc::new(
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
                active_agents: vec![agent.clone()],
                all_agents: vec![agent.clone()],
                failed_files: Vec::new(),
                allowed_agent_types: None,
            },
        );
        let store = crate::state::store::AppStore::new(app_state, None);
        let messages = vec![Message::Assistant(
            crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "current history".to_string(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            },
        )];
        let restore = ResumeRestoreStores {
            session_id: Some("context-session".to_string()),
            ..ResumeRestoreStores::default()
        };
        let loaded_nested_memory_paths =
            std::collections::HashSet::from(["/project/.claude/CLAUDE.md".to_string()]);
        let context = build_repl_process_user_input_context(
            ToolPermissionContext::default(),
            &store,
            messages.clone(),
            restore,
            loaded_nested_memory_paths.clone(),
            crate::state::app_state_store::McpState::default(),
            &[],
            std::sync::Arc::new(Vec::new()),
            false,
            &crate::utils::system_prompt::CliSystemPromptOverrides::default(),
            Some(&agent),
            None,
            &crate::tool::InteractivePermissionSink::default(),
            &crate::utils::thinking::ThinkingConfig::Adaptive,
            crate::tool::ResponseLengthSink::default(),
        );

        assert_eq!(context.messages, messages);
        assert!(context.app_store.is_some());
        assert_eq!(
            context.resume_restore_stores.session_id.as_deref(),
            Some("context-session")
        );
        assert!(context.main_loop_model.is_some());
        assert_eq!(
            context.loaded_nested_memory_paths,
            loaded_nested_memory_paths
        );
        assert_eq!(context.agent_type.as_deref(), Some("reviewer"));
        assert_eq!(context.agent_definitions.active_agents, vec![agent]);
    }

    /// Maps to CC `REPL.tsx:3206-3208`
    /// `agentDefinitions: allowedAgentTypes ? { ...s.agentDefinitions,
    /// allowedAgentTypes } : s.agentDefinitions`, fed by the `resolveAgentTools`
    /// memo at `REPL.tsx:1203-1221`. This is the only site in CC that puts
    /// `allowedAgentTypes` on an `agentDefinitions` object, so it is the only
    /// place the `Agent(x,y)` frontmatter restriction can enter a turn.
    #[test]
    fn process_input_context_merges_allowed_agent_types_like_official_repl() {
        use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
        let mut lead = AgentDefinition::new(
            "lead",
            "Main thread agent",
            AgentDefinitionSource::ProjectSettings,
        );
        lead.tools = Some(vec![
            format!(
                "{}(reviewer)",
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME
            ),
            crate::tools::bash_tool::tool_name::BASH_TOOL_NAME.to_string(),
        ]);
        let reviewer = AgentDefinition::new(
            "reviewer",
            "Reviews code",
            AgentDefinitionSource::ProjectSettings,
        );
        let auditor = AgentDefinition::new(
            "auditor",
            "Audits code",
            AgentDefinitionSource::ProjectSettings,
        );

        let mut app_state = crate::state::app_state_store::AppState::default();
        app_state.agent_definitions = std::sync::Arc::new(
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
                active_agents: vec![reviewer.clone(), auditor.clone()],
                all_agents: vec![lead.clone(), reviewer, auditor],
                failed_files: Vec::new(),
                allowed_agent_types: None,
            },
        );
        let store = crate::state::store::AppStore::new(app_state, None);

        let build = |agent: Option<&AgentDefinition>| {
            build_repl_process_user_input_context(
                ToolPermissionContext::default(),
                &store,
                Vec::new(),
                ResumeRestoreStores::default(),
                std::collections::HashSet::new(),
                crate::state::app_state_store::McpState::default(),
                &[],
                std::sync::Arc::new(Vec::new()),
                false,
                &crate::utils::system_prompt::CliSystemPromptOverrides::default(),
                agent,
                None,
                &crate::tool::InteractivePermissionSink::default(),
                &crate::utils::thinking::ThinkingConfig::Adaptive,
                crate::tool::ResponseLengthSink::default(),
            )
        };

        let restricted = build(Some(&lead));
        assert_eq!(
            restricted.agent_definitions.allowed_agent_types,
            Some(vec!["reviewer".to_string()])
        );
        // The same memo call also narrows the pool (CC `REPL.tsx:1218`), so the
        // restriction and the tool list come from one `resolveAgentTools`.
        let mut tool_names = restricted
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        tool_names.sort();
        assert_eq!(
            tool_names,
            vec![
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME.to_string(),
                crate::tools::bash_tool::tool_name::BASH_TOOL_NAME.to_string(),
            ]
        );
        // CC merges into a fresh shallow copy; `AppState.agentDefinitions` never
        // gains the field (`AppStateStore.ts:505` has no `allowedAgentTypes`).
        assert_eq!(store.get().agent_definitions.allowed_agent_types, None);

        // No main-thread agent → CC's falsy branch hands back `s.agentDefinitions`
        // itself, untouched.
        let unrestricted = build(None);
        assert_eq!(unrestricted.agent_definitions.allowed_agent_types, None);
        assert!(std::sync::Arc::ptr_eq(
            &unrestricted.agent_definitions,
            &store.get().agent_definitions
        ));
    }

    /// The MCP-prompt slash path does not use the builder above: its context
    /// comes from `submit_processed_prompt_deferred_query`
    /// (`handle_prompt_submit.rs:110-127`), which calls
    /// `with_permission_context` and therefore starts with EMPTY
    /// `agent_definitions`. This drives the seeding that path applies instead.
    ///
    /// The pair asserted here is exactly what
    /// [`apply_repl_query_turn_context`] runs for that path: the builder-once
    /// seed, then the per-turn [`install_repl_prompt_tools`] every path shares.
    ///
    /// The first assertion is the regression itself: before the fix, an Agent
    /// spawned from a slash-command turn saw no agent definitions at all.
    #[test]
    fn deferred_query_context_gets_the_definitions_the_builder_would_have_given_it() {
        use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
        let mut lead = AgentDefinition::new(
            "lead",
            "Main thread agent",
            AgentDefinitionSource::ProjectSettings,
        );
        lead.tools = Some(vec![format!(
            "{}(reviewer)",
            crate::tools::agent_tool::constants::AGENT_TOOL_NAME
        )]);
        let reviewer = AgentDefinition::new(
            "reviewer",
            "Reviews code",
            AgentDefinitionSource::ProjectSettings,
        );
        let auditor = AgentDefinition::new(
            "auditor",
            "Audits code",
            AgentDefinitionSource::ProjectSettings,
        );
        let store_definitions = std::sync::Arc::new(
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
                active_agents: vec![reviewer.clone(), auditor.clone()],
                all_agents: vec![lead.clone(), reviewer, auditor],
                failed_files: Vec::new(),
                allowed_agent_types: None,
            },
        );

        // Exactly what the deferred path starts from.
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        assert!(
            context.agent_definitions.active_agents.is_empty(),
            "precondition: the deferred path's context starts empty"
        );

        seed_deferred_query_shell_context(
            &mut context,
            Some(&lead),
            std::collections::HashSet::from(["/project/.claude/CLAUDE.md".to_string()]),
        );
        install_repl_prompt_tools(&mut context, &store_definitions, &[], Some(&lead));

        assert_eq!(
            context.agent_definitions.active_agents.len(),
            2,
            "the definitions snapshot must reach the turn"
        );
        assert_eq!(
            context.agent_definitions.allowed_agent_types,
            Some(vec!["reviewer".to_string()])
        );
        assert_eq!(context.agent_type.as_deref(), Some("lead"));
        // The other builder-owned field the shell context lacks
        // (`build_repl_process_user_input_context` sets it too).
        assert_eq!(
            context.loaded_nested_memory_paths,
            std::collections::HashSet::from(["/project/.claude/CLAUDE.md".to_string()])
        );
        // Same `resolveAgentTools` call yields both halves (`REPL.tsx:1218-1219`),
        // so the installed pool reflects `lead`'s declaration.
        assert_eq!(
            context
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec![crate::tools::agent_tool::constants::AGENT_TOOL_NAME]
        );

        // Absent restriction still passes the store object through by identity,
        // same as the builder (`REPL.tsx:3206-3208`).
        let mut plain = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        seed_deferred_query_shell_context(&mut plain, None, std::collections::HashSet::new());
        install_repl_prompt_tools(&mut plain, &store_definitions, &[], None);
        assert!(std::sync::Arc::ptr_eq(
            &plain.agent_definitions,
            &store_definitions
        ));
        assert_eq!(plain.agent_type, None);
    }

    /// The per-turn refresh has ONE owner, [`apply_repl_query_turn_context`], so
    /// a field belonging to the turn contract cannot reach only some of REPL's
    /// deferred-query paths. Two context shapes reach it: the caller-built one
    /// (typed prompt / inbox / scheduled queue, all via
    /// [`build_repl_process_user_input_context`]) and the permission-only shell
    /// the MCP-prompt slash path gets from
    /// `submit_processed_prompt_deferred_query`
    /// (`handle_prompt_submit.rs:110-127`). After the refresh the two must carry
    /// the same contract.
    ///
    /// CC has no equivalent divergence to reproduce: `onQuery` rebuilds the
    /// whole context from the one REPL-supplied builder
    /// (`REPL.tsx:3674-3679`), so every path is fed by construction. This
    /// equality is the port's stand-in for that, and it is the defect `39a121b`
    /// fixed: drop a write from the shell seed and an Agent spawned out of an
    /// MCP-prompt slash turn stops seeing what the builder would have given it.
    #[test]
    fn query_turn_context_owner_gives_every_path_the_same_turn_contract() {
        use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
        let mut lead = AgentDefinition::new(
            "lead",
            "Main thread agent",
            AgentDefinitionSource::ProjectSettings,
        );
        lead.tools = Some(vec![
            format!(
                "{}(reviewer)",
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME
            ),
            crate::tools::bash_tool::tool_name::BASH_TOOL_NAME.to_string(),
        ]);
        let reviewer = AgentDefinition::new(
            "reviewer",
            "Reviews code",
            AgentDefinitionSource::ProjectSettings,
        );
        let auditor = AgentDefinition::new(
            "auditor",
            "Audits code",
            AgentDefinitionSource::ProjectSettings,
        );
        let mut app_state = crate::state::app_state_store::AppState::default();
        app_state.agent_definitions = std::sync::Arc::new(
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult {
                active_agents: vec![reviewer.clone(), auditor.clone()],
                all_agents: vec![lead.clone(), reviewer, auditor],
                failed_files: Vec::new(),
                allowed_agent_types: None,
            },
        );
        let store = crate::state::store::AppStore::new(app_state, None);

        let loaded_nested_memory_paths =
            std::collections::HashSet::from(["/project/.claude/CLAUDE.md".to_string()]);
        let restore = ResumeRestoreStores::default();
        let overrides = crate::utils::system_prompt::CliSystemPromptOverrides::default();
        let thinking = crate::utils::thinking::ThinkingConfig::Adaptive;
        let has_interruptible_tool_in_progress =
            Arc::new(std::sync::atomic::AtomicBool::new(false));
        let make_params = |context: ToolUseContext| crate::query::QueryParams {
            turn_id: "turn-1".to_string(),
            input: "/mcp-prompt".to_string(),
            messages: Vec::new(),
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: crate::constants::query_source::QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: context,
        };

        // Shape 1: the three paths that ran the builder before dispatching.
        let mut builder_params = make_params(build_repl_process_user_input_context(
            ToolPermissionContext::default(),
            &store,
            Vec::new(),
            restore.clone(),
            loaded_nested_memory_paths.clone(),
            crate::state::app_state_store::McpState::default(),
            &[],
            std::sync::Arc::new(Vec::new()),
            false,
            &overrides,
            Some(&lead),
            None,
            &crate::tool::InteractivePermissionSink::default(),
            &thinking,
            crate::tool::ResponseLengthSink::default(),
        ));
        let builder_replacement_state = apply_repl_query_turn_context(
            &mut builder_params,
            &store,
            ToolPermissionContext::default(),
            &[],
            crate::utils::tool_result_storage::ContentReplacementState::new(),
            &restore,
            None,
            Some(&lead),
            &[],
            &overrides,
            None,
            &crate::tool::InteractivePermissionSink::default(),
            &has_interruptible_tool_in_progress,
            &thinking,
            None,
        );

        // Shape 2: the MCP-prompt slash path's permission-only shell.
        let shell = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        assert!(
            shell.agent_definitions.active_agents.is_empty()
                && shell.loaded_nested_memory_paths.is_empty(),
            "precondition: the MCP-prompt path's context starts as a shell"
        );
        let mut shell_params = make_params(shell);
        let shell_replacement_state = apply_repl_query_turn_context(
            &mut shell_params,
            &store,
            ToolPermissionContext::default(),
            &[],
            crate::utils::tool_result_storage::ContentReplacementState::new(),
            &restore,
            None,
            Some(&lead),
            &[],
            &overrides,
            None,
            &crate::tool::InteractivePermissionSink::default(),
            &has_interruptible_tool_in_progress,
            &thinking,
            Some(loaded_nested_memory_paths.clone()),
        );

        let builder = &builder_params.tool_use_context;
        let shell = &shell_params.tool_use_context;
        assert!(builder.set_has_interruptible_tool_in_progress.0.is_some());
        assert!(shell.set_has_interruptible_tool_in_progress.0.is_some());
        (builder
            .set_has_interruptible_tool_in_progress
            .0
            .as_ref()
            .expect("owner installs interruptible-tool sink"))(true);
        assert!(has_interruptible_tool_in_progress.load(std::sync::atomic::Ordering::SeqCst));
        (shell
            .set_has_interruptible_tool_in_progress
            .0
            .as_ref()
            .expect("owner installs sink for shell context"))(false);
        assert!(!has_interruptible_tool_in_progress.load(std::sync::atomic::Ordering::SeqCst));
        // The regression itself: the definitions snapshot and the `Agent(x,y)`
        // restriction must reach the MCP-prompt turn too.
        assert_eq!(
            shell.agent_definitions.active_agents,
            builder.agent_definitions.active_agents
        );
        assert_eq!(shell.agent_definitions.active_agents.len(), 2);
        assert_eq!(shell.agent_type, builder.agent_type);
        // The `Agent(x,y)` restriction is now the owner's, not the shell seed's:
        // `attach_app_store` re-reads `agent_definitions` from a store that
        // deliberately never holds the overlay (`AppStateStore.ts:505` has no
        // `allowedAgentTypes`; asserted by
        // `process_input_context_merges_allowed_agent_types_like_official_repl`),
        // so the owner re-applies it for BOTH shapes afterwards — CC's
        // `:3206-3208`, re-run by the second builder call at `:3674-3679`.
        assert_eq!(
            shell.agent_definitions.allowed_agent_types,
            builder.agent_definitions.allowed_agent_types,
            "the frontmatter restriction cannot depend on which path submitted"
        );
        assert_eq!(
            shell.agent_definitions.allowed_agent_types,
            Some(vec!["reviewer".to_string()])
        );
        assert_eq!(
            shell.loaded_nested_memory_paths,
            builder.loaded_nested_memory_paths
        );
        assert_eq!(shell.loaded_nested_memory_paths, loaded_nested_memory_paths);
        // …and so must every other field the owner writes.
        assert_eq!(
            shell.tool_permission_context,
            builder.tool_permission_context
        );
        assert_eq!(
            shell.content_replacement_state,
            builder.content_replacement_state
        );
        assert_eq!(shell.mcp_state, builder.mcp_state);
        assert_eq!(shell.resume_restore_stores, builder.resume_restore_stores);
        assert_eq!(
            shell
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            builder
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(shell.fast_mode, builder.fast_mode);
        assert_eq!(shell.thinking_config, builder.thinking_config);
        assert_eq!(shell.main_loop_model, builder.main_loop_model);
        assert_eq!(shell.effort_value, builder.effort_value);
        assert_eq!(builder_replacement_state, shell_replacement_state);
        assert_eq!(shell_params.messages, builder_params.messages);
        assert_eq!(shell_params.model_messages, builder_params.model_messages);
        assert_eq!(shell_params.system_prompt, builder_params.system_prompt);
        assert_eq!(shell_params.user_context, builder_params.user_context);
        assert_eq!(shell_params.system_context, builder_params.system_context);
    }

    /// The `Agent(x,y)` frontmatter restriction has gone missing three times
    /// (see the `repl_prompt_tools` module doc). This pins the contract that
    /// makes a fourth impossible to reach without failing a test: ONE owner,
    /// [`apply_repl_query_turn_context`], decides `agent_definitions` for every
    /// REPL deferred-query path, and it decides it the way CC's second
    /// `getToolUseContext` call does (`REPL.tsx:3674-3679`) — a FRESH store read
    /// (`:3165` → `:3207-3208` `s.agentDefinitions`) with the per-turn overlay
    /// re-applied on top (`:3206-3208`).
    ///
    /// Both halves are asserted, and they are what rule out the two wrong fixes:
    ///
    /// - Skipping the re-apply — the third recurrence, `attach_app_store`
    ///   (`tool.rs:1337`) overwriting a merged value — fails
    ///   `restriction on every path`.
    /// - Making that overwrite conditional instead ("keep what the caller set")
    ///   fails `store refresh must reach the turn`: the definitions snapshot
    ///   would freeze at whatever the pre-dispatch builder saw, and an `/agents`
    ///   reload mid-turn would be lost precisely on restricted turns. CC re-reads
    ///   it unconditionally.
    ///
    /// The four production paths reduce to two context shapes. The typed-prompt
    /// (`on_submit`), inbox-poller, and scheduled-queue callbacks each build a
    /// [`build_repl_process_user_input_context`] result, hand it to
    /// `submit_prompt_deferred_query_with_context{,_skipping_hooks}`
    /// (`handle_prompt_submit.rs:174-195`, `:210-230`) — both of which carry it
    /// straight onto `QueryParams.tool_use_context` via `into_deferred_query`
    /// (`:233`) — and call the owner with
    /// `shell_context_seed: None`. The MCP-prompt slash path instead gets the
    /// permission-only shell `submit_processed_prompt_deferred_query` mints
    /// (`handle_prompt_submit.rs:110-127`) and calls the owner with `Some(..)`.
    #[test]
    fn one_owner_decides_agent_definitions_for_every_query_path() {
        use crate::tools::agent_tool::load_agents_dir::{
            AgentDefinition, AgentDefinitionSource, AgentDefinitionsResult,
        };
        let mut lead = AgentDefinition::new(
            "lead",
            "Main thread agent",
            AgentDefinitionSource::ProjectSettings,
        );
        lead.tools = Some(vec![
            format!(
                "{}(reviewer)",
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME
            ),
            crate::tools::bash_tool::tool_name::BASH_TOOL_NAME.to_string(),
        ]);
        let reviewer = AgentDefinition::new(
            "reviewer",
            "Reviews code",
            AgentDefinitionSource::ProjectSettings,
        );
        let auditor = AgentDefinition::new(
            "auditor",
            "Audits code",
            AgentDefinitionSource::ProjectSettings,
        );
        let archivist = AgentDefinition::new(
            "archivist",
            "Loaded after the builder ran",
            AgentDefinitionSource::ProjectSettings,
        );

        let mut app_state = crate::state::app_state_store::AppState::default();
        app_state.agent_definitions = std::sync::Arc::new(AgentDefinitionsResult {
            active_agents: vec![reviewer.clone(), auditor.clone()],
            all_agents: vec![lead.clone(), reviewer.clone(), auditor.clone()],
            failed_files: Vec::new(),
            allowed_agent_types: None,
        });
        let store = crate::state::store::AppStore::new(app_state, None);

        let restore = ResumeRestoreStores::default();
        let overrides = crate::utils::system_prompt::CliSystemPromptOverrides::default();
        let thinking = crate::utils::thinking::ThinkingConfig::Adaptive;
        let has_interruptible_tool_in_progress =
            Arc::new(std::sync::atomic::AtomicBool::new(false));
        let loaded_nested_memory_paths =
            std::collections::HashSet::from(["/project/.claude/CLAUDE.md".to_string()]);
        let make_params = |context: ToolUseContext| crate::query::QueryParams {
            turn_id: "turn-1".to_string(),
            input: "hello".to_string(),
            messages: Vec::new(),
            model_messages: Vec::new(),
            system_prompt: Vec::new(),
            user_context: std::collections::BTreeMap::new(),
            system_context: std::collections::BTreeMap::new(),
            query_source: crate::constants::query_source::QuerySource::Prompt,
            token_budget: None,
            task_budget: None,
            max_turns: None,
            tool_use_context: context,
        };

        // Shape A — typed prompt / inbox / scheduled queue: the builder runs
        // BEFORE slash dispatch, so its merge is one dispatch old by the time
        // the owner runs.
        let builder_shape = |agent: Option<&AgentDefinition>| {
            make_params(build_repl_process_user_input_context(
                ToolPermissionContext::default(),
                &store,
                Vec::new(),
                restore.clone(),
                loaded_nested_memory_paths.clone(),
                crate::state::app_state_store::McpState::default(),
                &[],
                std::sync::Arc::new(Vec::new()),
                false,
                &overrides,
                agent,
                None,
                &crate::tool::InteractivePermissionSink::default(),
                &thinking,
                crate::tool::ResponseLengthSink::default(),
            ))
        };
        // Shape B — MCP-prompt slash path: no builder at all.
        let shell_shape = || {
            make_params(ToolUseContext::with_permission_context(
                ToolPermissionContext::default(),
            ))
        };

        let mut restricted = vec![
            (builder_shape(Some(&lead)), None),
            (shell_shape(), Some(loaded_nested_memory_paths.clone())),
        ];

        // The store gains a definition AFTER the builders above ran and BEFORE
        // the owner runs — CC's async `loadAgentsDir` result landing mid-turn.
        // Whatever the owner does to the overlay, the SNAPSHOT must still be a
        // fresh read.
        store.replace_with(|state| {
            let mut definitions = state.agent_definitions.as_ref().clone();
            definitions.active_agents.push(archivist.clone());
            definitions.all_agents.push(archivist.clone());
            state.agent_definitions = std::sync::Arc::new(definitions);
        });

        for (params, shell_context_seed) in &mut restricted {
            let _ = apply_repl_query_turn_context(
                params,
                &store,
                ToolPermissionContext::default(),
                &[],
                crate::utils::tool_result_storage::ContentReplacementState::new(),
                &restore,
                None,
                Some(&lead),
                &[],
                &overrides,
                None,
                &crate::tool::InteractivePermissionSink::default(),
                &has_interruptible_tool_in_progress,
                &thinking,
                shell_context_seed.clone(),
            );
        }

        for (params, seed) in &restricted {
            let path = if seed.is_some() {
                "MCP-prompt"
            } else {
                "builder-fed"
            };
            let definitions = &params.tool_use_context.agent_definitions;
            // restriction on every path — CC `REPL.tsx:3206-3208`.
            assert_eq!(
                definitions.allowed_agent_types,
                Some(vec!["reviewer".to_string()]),
                "{path} path dropped the Agent(x,y) frontmatter restriction"
            );
            // store refresh must reach the turn — CC `REPL.tsx:3165` + `:3207-3208`.
            assert_eq!(
                definitions
                    .active_agents
                    .iter()
                    .map(|agent| agent.agent_type.as_str())
                    .collect::<Vec<_>>(),
                vec!["reviewer", "auditor", "archivist"],
                "{path} path carried a stale definitions snapshot"
            );
        }

        // The overlay is turn-local: `AppStateStore.ts:505` has no
        // `allowedAgentTypes`, so two turns must not accumulate it.
        assert_eq!(store.get().agent_definitions.allowed_agent_types, None);

        // No main-thread agent → CC's falsy branch (`:3208`) hands back
        // `s.agentDefinitions` itself, so the owner must not manufacture a copy.
        let mut unrestricted = vec![
            (builder_shape(None), None),
            (shell_shape(), Some(loaded_nested_memory_paths.clone())),
        ];
        for (params, shell_context_seed) in &mut unrestricted {
            let _ = apply_repl_query_turn_context(
                params,
                &store,
                ToolPermissionContext::default(),
                &[],
                crate::utils::tool_result_storage::ContentReplacementState::new(),
                &restore,
                None,
                None,
                &[],
                &overrides,
                None,
                &crate::tool::InteractivePermissionSink::default(),
                &has_interruptible_tool_in_progress,
                &thinking,
                shell_context_seed.clone(),
            );
        }
        for (params, seed) in &unrestricted {
            let path = if seed.is_some() {
                "MCP-prompt"
            } else {
                "builder-fed"
            };
            assert!(
                std::sync::Arc::ptr_eq(
                    &params.tool_use_context.agent_definitions,
                    &store.get().agent_definitions
                ),
                "{path} path lost the identity pass-through with no restriction"
            );
        }
    }

    #[test]
    fn apply_repl_tool_use_context_options_matches_official_launch_budget() {
        let mut context = crate::tool::ToolUseContext::default();
        apply_repl_tool_use_context_options(
            &mut context,
            None,
            &crate::tool::InteractivePermissionSink::default(),
            false,
            Some(true),
            &crate::utils::thinking::ThinkingConfig::Enabled {
                budget_tokens: Some(8000),
            },
            None,
            None,
        );
        assert_eq!(
            context.thinking_config,
            Some(crate::utils::thinking::ThinkingConfig::Enabled {
                budget_tokens: Some(8000),
            })
        );
    }

    #[test]
    fn push_or_replace_entry_updates_streaming_row_by_id() {
        let mut entries = vec![HistoryEntry::Row(RenderableMessage::user("u1", "prompt"))];
        push_or_replace_entry(
            &mut entries,
            HistoryEntry::Row(RenderableMessage::system_notice(
                "stream-row",
                "partial",
                SystemMessageLevel::Info,
            )),
        );
        push_or_replace_entry(
            &mut entries,
            HistoryEntry::Row(RenderableMessage::system_notice(
                "stream-row",
                "complete",
                SystemMessageLevel::Info,
            )),
        );

        assert_eq!(entries.len(), 2);
        assert!(matches!(
            &entries[1],
            HistoryEntry::Row(RenderableMessage {
                kind: RenderableMessageKind::System(SystemMessage::Informational { content: text, .. }),
                ..
            }) if text == "complete"
        ));
    }

    #[test]
    fn remove_tombstoned_entry_removes_matching_uuid() {
        let mut entries = vec![
            HistoryEntry::Row(RenderableMessage::system("keep", "keep")),
            HistoryEntry::Row(RenderableMessage::system("drop", "drop")),
        ];
        remove_tombstoned_entry(&mut entries, "drop");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid(), "keep");
    }

    /// C3c-4: the seed's dual-carrier prefix and a live turn project back to
    /// exactly the seeded rows plus the live message's normalize output, and
    /// the model projection keeps arrival order with `Row` entries excluded.
    #[test]
    fn history_projections_split_seeded_and_live_entries_like_official_single_array() {
        let seeded_model = vec![Message::User(crate::types::message::UserMessage {
            uuid: "seed-user".to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text("hi".to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let seeded_rows = vec![RenderableMessage::user("seed-user", "hi")];
        let mut entries = seed_history_entries(&seeded_model, &seeded_rows);
        entries.push(HistoryEntry::Message(Message::User(
            crate::types::message::UserMessage {
                uuid: "live-user".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "follow-up".to_string(),
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
            },
        )));

        let rows = crate::utils::messages::project_history_rows(&entries);
        assert_eq!(
            rows.iter().map(|row| row.uuid.as_str()).collect::<Vec<_>>(),
            vec!["seed-user", "live-user"],
        );
        let model = crate::utils::messages::history_model_messages(&entries);
        assert_eq!(
            model.iter().map(Message::uuid).collect::<Vec<_>>(),
            vec!["seed-user", "live-user"],
        );
    }

    #[test]
    fn system_local_commands_enter_shared_typed_history_and_convert_for_api() {
        // All onDone(display:system) consumers share this boundary; branch is
        // not a special logging path. In particular, color/copy also persist.
        for command in ["branch", "color", "copy", "stats"] {
            let result = crate::utils::process_user_input::process_slash_command::system_display_local_command_result(
                None, command, "", "completed",
            );
            let expected_ids = result
                .messages
                .iter()
                .map(|row| row.uuid.clone())
                .collect::<Vec<_>>();
            let entries = result
                .messages
                .into_iter()
                .map(history_entry_from_row)
                .collect::<Vec<_>>();
            let messages = history_model_messages(&entries);
            assert_eq!(messages.len(), 2);
            assert!(messages.iter().all(|message| matches!(
                message,
                Message::System(SystemMessage::LocalCommand { .. })
            )));
            assert_eq!(
                messages.iter().map(Message::uuid).collect::<Vec<_>>(),
                expected_ids.iter().map(String::as_str).collect::<Vec<_>>()
            );
            let wire =
                crate::utils::session_storage::typed_messages_as_transcript_values(&messages);
            assert_eq!(wire.len(), 2);
            assert!(
                wire.iter()
                    .all(|entry| entry["type"] == "system" && entry["subtype"] == "local_command")
            );
            let first_timestamp = match &messages[0] {
                Message::System(message) => message.base().timestamp,
                _ => unreachable!(),
            };
            let api = crate::utils::messages::normalize_messages_for_api(messages);
            let [Message::User(user)] = api.as_slice() else {
                panic!(
                    "CC messages.ts:2078-2093 converts then merges both system rows into one user"
                );
            };
            assert_eq!(user.uuid, expected_ids[0]);
            assert_eq!(user.timestamp, first_timestamp);
            let text = user
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContent::Text(text) | UserContent::MetaText(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            assert_eq!(
                text.matches(&format!("<command-name>/{command}</command-name>"))
                    .count(),
                1
            );
            assert_eq!(
                text.matches("<local-command-stdout>completed</local-command-stdout>")
                    .count(),
                1
            );
        }
    }

    #[test]
    fn prompt_permissions_history_matches_official_attachment_and_audience() {
        // CC processSlashCommand.tsx:1247-1251 → REPL.tsx:3901 appends the
        // actual attachment to messages, not a UI-only projection.
        let attachment = crate::utils::attachments::Attachment::CommandPermissions {
            allowed_tools: Vec::new(),
            model: None,
        };
        let entries = vec![history_entry_from_row(RenderableMessage {
            uuid: "init-command-permissions".into(),
            kind: RenderableMessageKind::Attachment(attachment.clone()),
        })];
        let messages = history_model_messages(&entries);
        assert!(matches!(messages.as_slice(), [Message::Attachment(message)]
            if message.uuid == "init-command-permissions" && message.attachment == attachment
        ));
        let rows = crate::utils::messages::project_history_rows(&entries);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].uuid, "init-command-permissions");
        let wire = crate::utils::session_storage::typed_messages_as_transcript_values(&messages);
        // CC sessionStorage.ts:4351-4367 intentionally withholds ordinary
        // attachments from external JSONL; internal retains them.
        if crate::utils::build_profile::build_audience().is_internal() {
            assert_eq!(wire.len(), 1);
            assert_eq!(wire[0]["uuid"], "init-command-permissions");
            assert_eq!(wire[0]["attachment"]["type"], "command_permissions");
        } else {
            assert!(wire.is_empty());
        }
    }

    /// Maps to: CC `REPL.tsx:4904-4920` `rewindConversationTo` — slicing to
    /// just before the selected message; the projected-row uuid addresses the
    /// entry even when the seed prefix is dual-carrier.
    #[test]
    fn truncate_history_before_row_cuts_at_the_projected_row_entry() {
        let entries = vec![
            HistoryEntry::Row(RenderableMessage::user("u1", "first")),
            HistoryEntry::ModelOnly(Message::User(crate::types::message::UserMessage {
                uuid: "m1".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "model-only".to_string(),
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
            })),
            HistoryEntry::Message(Message::User(crate::types::message::UserMessage {
                uuid: "u2".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "second".to_string(),
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
            })),
        ];

        let truncated =
            truncate_history_before_row(&entries, "u2").expect("selected row must resolve");
        assert_eq!(truncated.len(), 2);
        assert_eq!(truncated[0].uuid(), "u1");
        assert!(truncate_history_before_row(&entries, "missing").is_none());
    }

    fn write_bomb_fixture(content_lines: usize, line_len: usize) -> std::path::PathBuf {
        // Minimal 3-entry session mirroring the real /resume blank-ocean
        // trigger: a restored Write tool_use whose rendered title contains a
        // long *word-breakable* file path (slashes/dashes are break points).
        // iocraft's Text measure answered Taffy's MinContent query by
        // re-wrapping at width 1, reporting ~one row per character; that
        // min-content height leaked into the rows container as a blank tail.
        // A same-length unbreakable path does NOT trigger (single unbreakable
        // word stays one line at min-content width).
        let synthetic = vec!["x".repeat(line_len); content_lines].join("\n");
        // Synthetic path: only its length (102) and its break points matter.
        let long_path = "/home/user/workspace/project-tree/generated-scripts/apply-a-deliberately-long-hyphenated-file-name.ps1";
        let sid = "ffffffff-bomb-0000-0000-000000000001";
        let bomb = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_bomb", "name": "Write",
                "input": {"file_path": long_path, "content": synthetic}
            }]},
            "uuid": "u-bomb", "parentUuid": null,
            "timestamp": "2026-06-10T07:56:03.037Z", "sessionId": sid,
            "cwd": "/tmp", "version": "2.1.170", "gitBranch": "HEAD",
            "isSidechain": false, "userType": "external"
        });
        let result = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_bomb",
                "content": format!("File created successfully at: {long_path}")
            }]},
            "toolUseResult": {
                "type": "create", "filePath": long_path,
                "content": synthetic, "structuredPatch": [],
                "originalFile": null, "userModified": false
            },
            "uuid": "u-result", "parentUuid": "u-bomb",
            "timestamp": "2026-06-10T07:56:04.000Z", "sessionId": sid
        });
        let path = std::env::temp_dir().join(format!("{sid}.jsonl"));
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({"type": "permission-mode", "permissionMode": "default", "sessionId": sid}),
                bomb,
                result
            ),
        )
        .expect("fixture write");
        path
    }

    #[component]
    fn WriteBombProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let current_theme = *theme::current();
        let (cols, rows) = hooks.use_terminal_size();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 4 {
            tick += 1;
        } else {
            system.exit();
        }

        let main_screen_width = cols.max(1) as u32;
        let messages = hooks.use_const(|| {
            let path = write_bomb_fixture(560, 35);
            let selection = SessionSelection {
                session_id: "ffffffff-bomb-0000-0000-000000000001".to_string(),
                project_path: None,
                file_path: path,
            };
            let target =
                resume::load_for_picker_selection(&selection).expect("fixture should load");
            let loaded = crate::utils::session_restore::ResumeLoadResult::try_from(&target)
                .expect("fixture should deserialize");
            let result = crate::utils::session_restore::process_resumed_conversation(
                loaded,
                None,
                Arc::new(
                    crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default(),
                ),
            )
            .expect("fixture should process");
            result.renderable_messages.clone()
        });

        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                // MessagesImpl reads AppState for the two expand flags.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                            #(memoized_messages(
                                Arc::clone(&messages),
                                0,
                                false,
                                false,
                                false,
                                cols,
                                rows,
                                None,
                                ClassifierApprovalsState::default(),
                                StatusNoticeContext::default(),
                                None,
                                Arc::new(std::collections::HashSet::new()),
                                Arc::new(std::collections::HashSet::new()),
                                Arc::new(Vec::new()),
                            ))
                            // Footer stand-in: keeps any layout bloat *between* the
                            // transcript and the last row (like the real PromptInput),
                            // where the inline trailing-blank-row trim cannot reach it.
                            Text(content: "footer-probe")
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[test]
    fn resume_write_bomb_canvas_has_no_min_content_bloat() {
        let canvases = futures::executor::block_on(
            element!(WriteBombProbe)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(125, 40))
                .collect::<Vec<_>>(),
        );
        let text = canvases
            .last()
            .expect("mock render should produce a final canvas")
            .to_string();
        let line_count = text.lines().count();

        // Expected: logo + "Wrote 560 lines" row + 10-line preview +
        // "+550 lines" marker ≈ well under 60 rows. The blank-ocean bug
        // inflates this with a ~104-row bloat region.
        assert!(
            line_count < 60,
            "restored Write tool_use should render truncated; got {line_count} lines:\n{}",
            text.lines()
                .enumerate()
                .map(|(i, l)| format!("{i:4}|{l}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[derive(Default, Props)]
    struct IsolatedAuthReplProps {
        initial_messages: Option<Arc<Vec<Message>>>,
        initial_renderable_messages: Option<Arc<Vec<RenderableMessage>>>,
        /// Contract B clause 2: harnesses mount the real `AppStateProvider`
        /// instead of injecting a raw `AppStore` context, so the store travels
        /// as a prop. `None` keeps the previous provider-less default (a store
        /// seeded with the on-disk permission context) without any production
        /// fallback in `Repl`.
        app_store: Option<crate::state::store::AppStore>,
    }

    struct ScopedReplAuthConfig {
        previous: Option<crate::utils::config::GlobalConfig>,
    }

    impl ScopedReplAuthConfig {
        fn disable_anthropic_auth() -> Self {
            let mut config = crate::utils::config::load_global_config();
            config.api_key_helper = Some("repl-render-test-auth-source".to_string());
            Self {
                previous: crate::utils::config::replace_test_global_config(Some(config)),
            }
        }
    }

    impl Drop for ScopedReplAuthConfig {
        fn drop(&mut self) {
            let _ = crate::utils::config::replace_test_global_config(self.previous.take());
        }
    }

    /// Scoped thread-local auth guard for REPL render tests. A configured API
    /// key helper makes canonical Anthropic auth disabled, so startup reverify
    /// exits before executing it. Drop restores any enclosing test override.
    #[component]
    fn IsolatedAuthRepl(
        props: &IsolatedAuthReplProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let _auth_config =
            hooks.use_const(|| Arc::new(ScopedReplAuthConfig::disable_anthropic_auth()));
        // Built unconditionally so no hook is seeded from props (iocraft may
        // first-mount with `Default` props); harnesses that pass their own
        // store simply discard it.
        let default_store = hooks.use_const(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            // Test-only startup fixture. Production /clear preserves the live
            // permission context; initialization belongs to main.tsx.
            let permission = crate::utils::process_runtime::block_on_from_sync(async {
                crate::utils::permissions::permission_setup::initialize_tool_permission_context(
                    &crate::utils::settings::get_initial_settings(),
                    &crate::utils::permissions::permissions_loader::load_all_permission_rules_from_disk(),
                    &[], &[], None, &[],
                ).await
            }).unwrap().unwrap().tool_permission_context;
            initial.set_tool_permission_context(permission);
            crate::state::store::AppStore::new(initial, None)
        });
        let app_store = props.app_store.clone().unwrap_or(default_store);
        let initial_messages = props.initial_messages.clone();
        let initial_renderable_messages = props.initial_renderable_messages.clone();
        element! {
            crate::state::app_state::AppStateProvider(
                prebuilt_store: Some(app_store),
                children: crate::state::app_state::ProviderChildren::new(move || {
                    element! {
                        Repl(
                            initial_messages: initial_messages.clone(),
                            initial_renderable_messages: initial_renderable_messages.clone(),
                        )
                    }
                    .into_any()
                }),
            )
        }
    }

    #[component]
    fn ReplHarness() -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        element! {
            ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                ContextProvider(value: Context::owned(current_theme)) {
                    IsolatedAuthRepl
                }
            }
        }
    }

    #[component]
    fn ReplCompletedSpeculationHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let app_store = hooks.use_const(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.prompt_suggestion_enabled = true;
            initial.prompt_suggestion = crate::state::app_state_store::PromptSuggestionState {
                text: Some("continue the work".to_string()),
                prompt_id: Some("prompt-spec".to_string()),
                shown_at: 1,
                accepted_at: 0,
                generation_request_id: None,
            };
            let store = crate::state::store::AppStore::new(initial, None);
            let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
            let cache = Arc::new(crate::utils::forked_agent::CacheSafeParams {
                system_prompt: vec!["system".to_string()],
                user_context: Default::default(),
                system_context: Default::default(),
                tool_use_context: context,
                fork_context_messages: Arc::new(Vec::new()),
            });
            store.replace_with(|state| {
                state.speculation = crate::state::app_state_store::SpeculationState::Active(
                    crate::state::app_state_store::ActiveSpeculationState {
                        id: uuid::Uuid::new_v4().simple().to_string(),
                        abort_controller: crate::tool::AbortController::default(),
                        start_time: current_time_millis().saturating_sub(100),
                        messages: Arc::new(std::sync::Mutex::new(vec![Message::Assistant(
                            crate::types::message::AssistantMessage {
                                uuid: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now(),
                                content: vec![crate::types::message::AssistantContent::Text(
                                    "already speculated reply".to_string(),
                                )],
                                model: None,
                                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                                usage: None,
                            },
                        )])),
                        written_paths: Arc::new(std::sync::Mutex::new(Default::default())),
                        boundary: Some(
                            crate::state::app_state_store::CompletionBoundary::Complete {
                                completed_at: current_time_millis(),
                                output_tokens: 5,
                            },
                        ),
                        suggestion_length: 17,
                        tool_use_count: 0,
                        is_pipelined: false,
                        cache_safe_params: cache,
                        pipelined_suggestion: Some(
                            crate::state::app_state_store::PipelinedSuggestion {
                                text: "run the final checks".to_string(),
                                prompt_id: "user_intent".to_string(),
                                generation_request_id: Some("req_pipeline".to_string()),
                            },
                        ),
                    },
                );
            });
            store
        });
        element! {
            ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                ContextProvider(value: Context::owned(current_theme)) {
                    IsolatedAuthRepl(app_store: Some(app_store.clone()))
                }
            }
        }
    }

    #[component]
    fn ReplCommandKeybindingHarness() -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let mut bindings = crate::keybindings::default_bindings::default_bindings();
        bindings.push(crate::keybindings::types::ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("ctrl+y"),
            action: Some("command:help".to_string()),
            context: crate::keybindings::types::ContextName::Chat,
        });
        let runtime = crate::keybindings::keybinding_context::KeybindingRuntime::new(bindings);
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(current_theme)) {
                    IsolatedAuthRepl
                }
            }
        }
    }

    #[component]
    fn ReplLogoHotPathHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        // Force parent-only idle updates before terminal input arrives. The
        // LogoHeader memo boundary must retain its subtree across these frames.
        let mut idle_tick = hooks.use_state(|| 0u8);
        if idle_tick.get() < 2 {
            idle_tick += 1;
        }
        let current_theme = *theme::current();
        element! {
            ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                ContextProvider(value: Context::owned(current_theme)) {
                    IsolatedAuthRepl
                }
            }
        }
    }

    #[component]
    fn ReplSpinnerOwnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let app_store = hooks.use_const(|| {
            let mut settings = crate::utils::settings::types::SettingsJson::default();
            settings.spinner_verbs = Some(crate::utils::settings::types::SpinnerVerbsSettings {
                mode: "replace".to_string(),
                verbs: vec!["Repl-owned-spinner".to_string()],
            });
            settings.prefers_reduced_motion = Some(true);
            let mut state = crate::state::app_state_store::AppState::default();
            state.settings = Arc::new(settings);
            crate::state::store::AppStore::new(state, None)
        });
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                IsolatedAuthRepl(app_store: Some(app_store.clone()))
            }
        }
    }

    #[component]
    fn ReplRunningTeammateSpinnerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let app_store = hooks.use_const(|| {
            let mut settings = crate::utils::settings::types::SettingsJson::default();
            settings.prefers_reduced_motion = Some(true);
            let mut state = crate::state::app_state_store::AppState::default();
            state.settings = Arc::new(settings);
            let task = crate::components::spinner::teammate_tree::TeammateTaskSnapshot {
                id: "task-reviewer".to_string(),
                task_type: crate::components::spinner::teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE
                    .to_string(),
                status: crate::components::spinner::teammate_tree::RUNNING_TASK_STATUS.to_string(),
                agent_name: "reviewer".to_string(),
                ..Default::default()
            };
            Arc::make_mut(&mut state.tasks).insert(
                task.id.clone(),
                Arc::new(crate::state::app_state_store::TaskState::InProcessTeammate(
                    task,
                )),
            );
            crate::state::store::AppStore::new(state, None)
        });
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                IsolatedAuthRepl(app_store: Some(app_store.clone()))
            }
        }
    }

    #[derive(Default, Props)]
    struct ReplInitialMessagesHarnessProps {
        initial_messages: Arc<Vec<Message>>,
        initial_renderable_messages: Arc<Vec<RenderableMessage>>,
    }

    #[component]
    fn ReplInitialMessagesHarness(
        props: &ReplInitialMessagesHarnessProps,
    ) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(current_theme)) {
                    IsolatedAuthRepl(
                        initial_messages: Some(props.initial_messages.clone()),
                        initial_renderable_messages: Some(props.initial_renderable_messages.clone()),
                    )
                }
            }
        }
    }

    #[test]
    fn repl_initial_messages_render_on_first_frame_like_official_resume_props() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let entries = vec![json!({
            "type": "user",
            "uuid": "resume-user",
            "timestamp": "2026-07-12T00:00:00.000Z",
            "message": {"role": "user", "content": "first-frame resumed prompt"}
        })];
        let initial_messages = Arc::new(conversation::into_typed_messages(entries.clone()));
        let initial_renderable_messages = Arc::new(
            conversation_recovery::renderable_messages_from_entries(&entries),
        );
        let canvases = futures::executor::block_on(
            element!(ReplInitialMessagesHarness(
                initial_messages,
                initial_renderable_messages,
            ))
            .mock_terminal_render_loop(MockTerminalConfig::default().with_size(100, 30))
            .take(1)
            .collect::<Vec<_>>(),
        );
        let canvas = canvases.first().expect("initial frame").to_string();
        assert!(
            canvas.contains("first-frame resumed prompt"),
            "canvas=\n{canvas}"
        );
    }

    #[test]
    fn rewind_composer_matches_official_restore_and_edited_remount() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _checkpointing = crate::utils::env_utils::EnvVarGuard::set(
            "CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING",
            "0",
        );
        let entries = vec![
            json!({"type":"user","uuid":"rewind-first","timestamp":"2026-07-12T00:00:00.000Z","message":{"role":"user","content":"first prompt"}}),
            json!({"type":"user","uuid":"rewind-second","timestamp":"2026-07-12T00:00:01.000Z","message":{"role":"user","content":"<ide_selection>noise</ide_selection>\n<code>second</code>"}}),
        ];
        let initial_messages = Arc::new(conversation::into_typed_messages(entries.clone()));
        let initial_renderable_messages = Arc::new(
            conversation_recovery::renderable_messages_from_entries(&entries),
        );
        let text = futures::executor::block_on(async {
            let mut app = element!(ReplInitialMessagesHarness(
                initial_messages,
                initial_renderable_messages
            ));
            let events = timed_stream(vec![
                (key(KeyCode::Esc), 180),
                (key(KeyCode::Esc), 60),
                (key(KeyCode::Up), 120),
                (key(KeyCode::Enter), 120),
                (key(KeyCode::Enter), 120),
                (key(KeyCode::Char('X')), 200),
                (ctrl_key('o'), 150),
                (ctrl_key('o'), 150),
                (key(KeyCode::F(24)), 150),
            ]);
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
            let started = Instant::now();
            let mut last = String::new();
            let mut saw_confirmation = false;
            while started.elapsed() < Duration::from_secs(5) {
                let next = crate::utils::race(frames.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(400)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas_lines(&canvas).join("\n");
                saw_confirmation |= last.contains("Confirm you want to restore");
                if started.elapsed() > Duration::from_millis(1400)
                    && last.contains("<code>second</code>X")
                {
                    break;
                }
            }
            assert!(saw_confirmation, "confirmation never appeared:\n{last}");
            last
        });
        assert!(
            text.contains("<code>second</code>X"),
            "restored edit lost after remount:\n{text}"
        );
        assert!(
            !text.contains("noise"),
            "IDE context leaked into composer:\n{text}"
        );
    }

    #[test]
    fn restore_message_sync_matches_official_image_ids_and_optional_setters() {
        use crate::components::prompt_input::input_paste::PastedContent;
        let mut message =
            crate::utils::messages::create_user_message("<bash-input>pwd</bash-input>".to_string());
        message.image_paste_ids = Some(vec![7]);
        for data in ["aA==", "YQ=="] {
            message
                .content
                .push(crate::types::message::UserContent::Image {
                    media_type: "image/png".to_string(),
                    data: data.to_string(),
                });
        }
        let update = restore_message_sync(&message, 3, "draft");
        assert_eq!(update.text, "pwd");
        assert_eq!(
            update.mode,
            Some(crate::components::prompt_input::input_modes::PromptInputMode::Bash)
        );
        let images = update.pasted_contents.unwrap();
        assert!(
            matches!(&images[&7], PastedContent::Image { data: Some(data), .. } if data == "aA==")
        );
        assert!(images.contains_key(&2));
        message.content.clear();
        let unchanged = restore_message_sync(&message, 4, "draft");
        assert_eq!(unchanged.text, "draft");
        assert!(unchanged.mode.is_none() && unchanged.pasted_contents.is_none());
    }

    #[test]
    fn transcript_keybindings_toggle_cap_expand_and_return_to_prompt_matches_official_identity() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _fullscreen = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let prompt_path = "identity-fixture/verbose-only.rs";
        let absolute_path = crate::bootstrap::state::get_original_cwd()
            .join(prompt_path)
            .display()
            .to_string();
        // Keep the verbose-sensitive Edit row above enough trailing rows to
        // place it in native scrollback. That makes the regression exercise
        // OffscreenFreeze state, rather than merely proving that a visible row
        // responds to changed verbose props.
        let mut renderable = vec![RenderableMessage::assistant_block(
            "transcript-edit",
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu-transcript-edit".to_string()),
                name: "Edit".to_string(),
                input: serde_json::json!({
                    "file_path": absolute_path.clone(),
                    "old_string": "before",
                    "new_string": "after"
                }),
            }),
        )];
        renderable.extend((0..35).map(|index| {
            RenderableMessage::user(
                format!("transcript-{index}"),
                format!("transcript row {index}"),
            )
        }));
        let renderable = Arc::new(renderable);
        let events = timed_stream(vec![
            (ctrl_key('o'), 20),
            (ctrl_key('e'), 120),
            (ctrl_key('o'), 120),
        ]);
        let frames = futures::executor::block_on(async move {
            let mut app = element!(ReplInitialMessagesHarness(
                initial_messages: Arc::new(Vec::new()),
                initial_renderable_messages: renderable,
            ));
            // Height 20 keeps the Edit row offscreen; width 200 must hold the
            // gate workdir's verbose absolute path on one row, because until
            // the text-flow port (#78) the flex header truncates the
            // FilePathLink box instead of wrapping the whole line like CC.
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(200, 20),
            ));
            let mut frames = Vec::new();
            for _ in 0..48 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(200)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                frames.push(canvas.to_string());
            }
            frames
        });
        let collapsed = frames
            .iter()
            .find(|frame| frame.contains("ctrl+e to show all"))
            .expect("collapsed transcript frame");
        assert!(
            collapsed.contains("transcript row 34"),
            "canvas=\n{collapsed}"
        );
        assert!(
            !collapsed.contains("transcript row 0"),
            "collapsed transcript keeps only the latest 30 rows; canvas=\n{collapsed}"
        );
        assert!(
            collapsed.contains("ctrl+e to show 6 previous messages"),
            "collapsed transcript should render the Messages-owned truncation divider; canvas=\n{collapsed}"
        );
        let expanded = frames
            .iter()
            .find(|frame| frame.contains("ctrl+e to collapse"))
            .expect("expanded transcript frame");
        assert!(expanded.contains("transcript row 0"), "canvas=\n{expanded}");
        assert!(
            expanded.contains("ctrl+e to hide 6 previous messages"),
            "expanded transcript should render the Messages-owned hide divider; canvas=\n{expanded}"
        );
        assert!(
            expanded.contains(&absolute_path),
            "expanded transcript should use verbose absolute-path formatting; canvas=\n{expanded}"
        );
        let last = frames.last().expect("prompt frame after transcript exit");
        assert!(
            !last.contains("Showing detailed transcript"),
            "canvas=\n{last}"
        );
        assert!(last.contains("? for shortcuts"), "canvas=\n{last}");
        assert!(
            last.contains(prompt_path),
            "prompt return should restore short-path formatting; canvas=\n{last}"
        );
        assert!(
            !last.contains(&absolute_path),
            "transcript OffscreenFreeze state leaked verbose formatting into prompt; canvas=\n{last}"
        );
    }

    #[component]
    fn ReplElicitationHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        let elicitation_state = hooks.use_state(|| {
            let mut initial = crate::state::app_state_store::AppState::default();
            initial.elicitation =
                std::sync::Arc::new(crate::state::app_state_store::ElicitationState {
                    queue: vec![
                    crate::services::mcp::elicitation_handler::ElicitationRequestEvent::new(
                        "docs",
                        "request-1",
                        crate::services::mcp::elicitation_handler::ElicitationRequestParams::Url {
                            message: "Authorize docs".to_string(),
                            url: "https://example.com/auth".to_string(),
                            elicitation_id: Some("elicit-1".to_string()),
                        },
                    ),
                ],
                });
            crate::state::store::AppStore::new(initial, None)
        });
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                IsolatedAuthRepl(app_store: Some(elicitation_state.read().clone()))
            }
        }
    }

    #[test]
    fn repl_renders_elicitation_queue_dialog_from_app_state_boundary() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let canvases = futures::executor::block_on(
            element!(ReplElicitationHarness)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(120, 30))
                .take(1)
                .collect::<Vec<_>>(),
        );
        let text = canvases
            .last()
            .expect("mock render should produce elicitation dialog")
            .to_string();
        assert!(
            text.contains("MCP server “docs” wants to open a URL"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Authorize docs"), "canvas=\n{text}");
    }

    #[derive(Default, Props)]
    struct ReplStartupDialogHarnessProps {
        snapshot: ReplStartupDialogSnapshot,
    }

    #[component]
    fn ReplStartupDialogHarness(
        props: &ReplStartupDialogHarnessProps,
    ) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ContextProvider(value: Context::owned(props.snapshot.clone())) {
                    IsolatedAuthRepl
                }
            }
        }
    }

    #[derive(Default, Props)]
    struct ReplStatusNoticeHarnessProps {
        context: StatusNoticeContext,
    }

    #[component]
    fn ReplStatusNoticeHarness(
        props: &ReplStatusNoticeHarnessProps,
    ) -> impl Into<AnyElement<'static>> {
        let current_theme = *theme::current();
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ContextProvider(value: Context::owned(props.context.clone())) {
                    IsolatedAuthRepl
                }
            }
        }
    }

    #[component]
    fn RestoredSessionLogoProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let current_theme = *theme::current();
        let (cols, rows) = hooks.use_terminal_size();
        if cols == 100 && rows == 35 {
            system.exit();
        }
        let main_screen_width = cols.max(1) as u32;
        let messages = hooks.use_const(|| {
            Arc::new(vec![RenderableMessage::user(
                "restored-1",
                "restored session row",
            )])
        });

        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                // MessagesImpl reads AppState for the two expand flags.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                            #(memoized_messages(
                                Arc::clone(&messages),
                                0,
                                false,
                                false,
                                false,
                                cols,
                                rows,
                                None,
                                ClassifierApprovalsState::default(),
                                StatusNoticeContext::default(),
                                None,
                                Arc::new(std::collections::HashSet::new()),
                                Arc::new(std::collections::HashSet::new()),
                                Arc::new(Vec::new()),
                            ))
                            // Footer stand-in: keeps any layout bloat *between* the
                            // transcript and the last row (like the real PromptInput),
                            // where the inline trailing-blank-row trim cannot reach it.
                            Text(content: "footer-probe")
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[component]
    fn LocalCommandUiRootProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let current_theme = *theme::current();
        let (cols, rows) = hooks.use_terminal_size();
        if cols == 125 && rows == 41 {
            system.exit();
        }
        let main_screen_width = cols.max(1) as u32;

        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                // Settings' Status tab reads `state.settings`, strict since P7.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                            ClearScreenOnGeneration(generation: 0u64)
                            Text(content: "transcript context stays visible".to_string())
                            Settings(on_close: move |_| {}, default_tab: Some(SettingsTab::Status))
                        }
                    }.into_any()),
                )
            }
        }
    }

    #[component]
    fn ImmediateLocalCommandUiDuringQueryProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let current_theme = *theme::current();
        let (cols, rows) = hooks.use_terminal_size();
        if cols == 100 && rows == 30 {
            system.exit();
        }
        let main_screen_width = cols.max(1) as u32;
        let messages = Arc::new(vec![RenderableMessage::user(
            "prompt-slow-query",
            "slow query active smoke",
        )]);

        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        View(flex_direction: FlexDirection::Column, width: main_screen_width) {
                            Messages(
                                messages: Arc::clone(&messages),
                                is_loading: true,
                                verbose: false,
                                screen: Screen::Prompt,
                                hide_logo: false,
                            )
                            Settings(on_close: move |_| {}, default_tab: Some(SettingsTab::Status))
                            PromptInput(
                                on_submit: move |_| {},
                                on_exit: move |_| {},
                                is_local_command_ui_active: true,
                                is_loading: true,
                                permission_mode: crate::types::permissions::PermissionMode::Default,
                                on_permission_mode_cycle: move |_| {},
                            )
                        }
                    }.into_any()),
                )
            }
        }
    }

    fn slash_invocation(command_name: &str, args: &str) -> SlashCommandInvocation {
        SlashCommandInvocation::new(command_name, args)
    }

    fn local_invocation(command_name: &str, args: &str) -> LocalCommandInvocation {
        LocalCommandInvocation {
            slash_command: slash_invocation(command_name, args),
            dismiss_result: None,
        }
    }

    fn env_lock() -> &'static crate::utils::env_utils::TestEnvLock {
        &crate::utils::env_utils::TEST_ENV_LOCK
    }

    struct OriginalCwdGuard {
        previous_original: PathBuf,
        previous_process: PathBuf,
    }

    impl OriginalCwdGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous_original = crate::bootstrap::state::get_original_cwd();
            let previous_process = std::env::current_dir().unwrap();
            crate::bootstrap::state::set_original_cwd(path);
            std::env::set_current_dir(path).unwrap();
            Self {
                previous_original,
                previous_process,
            }
        }
    }

    impl Drop for OriginalCwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous_process);
            crate::bootstrap::state::set_original_cwd(&self.previous_original);
        }
    }

    fn row_has_visible_cell(canvas: &Canvas, y: usize) -> bool {
        (0..canvas.width()).any(|x| {
            canvas
                .cell(x, y)
                .and_then(|cell| cell.text())
                .is_some_and(|text| !text.trim().is_empty())
        })
    }

    fn first_non_blank_row(canvas: &Canvas) -> Option<usize> {
        (0..canvas.height()).find(|&y| row_has_visible_cell(canvas, y))
    }

    fn last_non_blank_row(canvas: &Canvas) -> Option<usize> {
        (0..canvas.height())
            .rev()
            .find(|&y| row_has_visible_cell(canvas, y))
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

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn ctrl_key(ch: char) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(ch));
        event.modifiers = KeyModifiers::CONTROL;
        TerminalEvent::Key(event)
    }

    fn text_input_events(text: &str) -> Vec<TerminalEvent> {
        text.chars()
            .map(|ch| key(KeyCode::Char(ch)))
            .chain(std::iter::once(key(KeyCode::Enter)))
            .collect()
    }

    /// One step of an event-driven REPL script: wait until a frame rendered
    /// after the previous step's input contains every `wait_for` needle (an
    /// empty list waits for any such frame), then send `send`.
    struct ReplScriptStep {
        wait_for: Vec<String>,
        send: Vec<TerminalEvent>,
    }

    fn script_step(wait_for: &[&str], send: Vec<TerminalEvent>) -> ReplScriptStep {
        ReplScriptStep {
            wait_for: wait_for.iter().map(|needle| needle.to_string()).collect(),
            send,
        }
    }

    /// Drives `app` through `steps`, sending each step's input only once the
    /// UI it targets is on screen, then collects the frames the last input
    /// settles into. Unlike a `timed_stream` script, input can never race
    /// ahead of an asynchronously mounted view (an Enter sent before a picker
    /// finishes loading is consumed by whatever else is focused), so the
    /// script holds under any load. End each script with a step whose
    /// `wait_for` names the expected final state and whose `send` is empty.
    fn run_repl_script(mut app: AnyElement<'static>, steps: Vec<ReplScriptStep>) -> Vec<Canvas> {
        // Generous: only reached when the awaited state never renders.
        const STEP_BUDGET: Duration = Duration::from_secs(15);
        const SETTLE: Duration = Duration::from_millis(300);
        const MAX_FRAMES: usize = 400;
        let render_frames = |canvases: &[Canvas]| {
            canvases
                .iter()
                .map(canvas_lines)
                .map(|lines| lines.join("\n"))
                .collect::<Vec<_>>()
                .join("\n--- frame ---\n")
        };
        futures::executor::block_on(async {
            let (event_tx, event_rx) = futures::channel::mpsc::unbounded::<TerminalEvent>();
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(event_rx)));
            let mut canvases: Vec<Canvas> = Vec::new();
            let mut since = 0usize;
            for (index, step) in steps.into_iter().enumerate() {
                let deadline = std::time::Instant::now() + STEP_BUDGET;
                while !canvases[since..].iter().any(|canvas| {
                    let text = canvas_lines(canvas).join("\n");
                    step.wait_for.iter().all(|needle| text.contains(needle.as_str()))
                }) {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    let next = if remaining.is_zero() || canvases.len() >= MAX_FRAMES {
                        None
                    } else {
                        crate::utils::race(render_loop.next(), async move {
                            futures_timer::Delay::new(remaining).await;
                            None
                        })
                        .await
                    };
                    let Some(canvas) = next else {
                        panic!(
                            "script step {index} never rendered {:?}; frames=\n{}",
                            step.wait_for,
                            render_frames(&canvases)
                        );
                    };
                    canvases.push(canvas);
                }
                since = canvases.len();
                for event in step.send {
                    let _ = event_tx.unbounded_send(event);
                }
            }
            while canvases.len() < MAX_FRAMES {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(SETTLE).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
            }
            canvases
        })
    }

    fn collect_repl_canvases<S>(events: S, timeout_ms: u64, max_frames: usize) -> Vec<Canvas>
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        futures::executor::block_on(async {
            let mut app = element!(ReplHarness);
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(events)));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(timeout_ms)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= max_frames {
                    break;
                }
            }
            canvases
        })
    }

    fn collect_repl_command_keybinding_canvases<S>(
        events: S,
        timeout_ms: u64,
        max_frames: usize,
    ) -> Vec<Canvas>
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        futures::executor::block_on(async {
            let mut app = element!(ReplCommandKeybindingHarness);
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(events)));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(timeout_ms)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= max_frames {
                    break;
                }
            }
            canvases
        })
    }

    fn collect_repl_spinner_owner_canvases<S>(
        events: S,
        timeout_ms: u64,
        max_frames: usize,
    ) -> Vec<Canvas>
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        futures::executor::block_on(async {
            let mut app = element!(ReplSpinnerOwnerHarness);
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(events)));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(timeout_ms)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= max_frames {
                    break;
                }
            }
            canvases
        })
    }

    fn collect_repl_running_teammate_canvases<S>(
        events: S,
        timeout_ms: u64,
        max_frames: usize,
    ) -> Vec<Canvas>
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        futures::executor::block_on(async {
            let mut app = element!(ReplRunningTeammateSpinnerHarness);
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(events)));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(timeout_ms)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= max_frames {
                    break;
                }
            }
            canvases
        })
    }

    fn collect_repl_startup_dialog_canvases<S>(
        snapshot: ReplStartupDialogSnapshot,
        events: S,
        timeout_ms: u64,
        max_frames: usize,
    ) -> Vec<Canvas>
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        futures::executor::block_on(async {
            let mut app = element!(ReplStartupDialogHarness(snapshot: snapshot));
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(events)));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(timeout_ms)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= max_frames {
                    break;
                }
            }
            canvases
        })
    }

    fn collect_repl_status_notice_canvases<S>(
        context: StatusNoticeContext,
        events: S,
        timeout_ms: u64,
        max_frames: usize,
    ) -> Vec<Canvas>
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        futures::executor::block_on(async {
            let mut app = element!(ReplStatusNoticeHarness(context: context));
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(events)));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(timeout_ms)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= max_frames {
                    break;
                }
            }
            canvases
        })
    }

    fn timed_stream(
        events: Vec<(TerminalEvent, u64)>,
    ) -> impl Stream<Item = TerminalEvent> + Send + 'static {
        stream::unfold(events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        })
    }

    fn last_repl_text<S>(events: S, timeout_ms: u64, max_frames: usize) -> String
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        let canvases = collect_repl_canvases(events, timeout_ms, max_frames);
        let last = canvases
            .last()
            .expect("mock render should produce a canvas");
        canvas_lines(last).join("\n")
    }

    /// Frame collection with an EXPLICIT settle condition. The silence-based
    /// `last_repl_text` treats "no frame for timeout_ms" as settled, which
    /// under a saturated full-suite run fires between two frames of a still-
    /// rendering turn and hands the assertion an intermediate canvas (the
    /// resume_by_* family failed exactly this way: the "final" frame was the
    /// startup logo). Here collection stops when the newest frame contains
    /// `needle`, with `budget_ms` as the load-tolerant upper bound; on budget
    /// exhaustion the last frame is returned and the caller's assertion
    /// reports the mismatch.
    fn last_repl_text_waiting_for<S>(events: S, needle: &str, budget_ms: u64) -> String
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        futures::executor::block_on(async {
            let mut app = element!(ReplHarness);
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(events)));
            let deadline = std::time::Instant::now() + Duration::from_millis(budget_ms);
            let mut last_text = String::new();
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let next = crate::utils::race(render_loop.next(), async move {
                    futures_timer::Delay::new(remaining).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last_text = canvas_lines(&canvas).join("\n");
                if last_text.contains(needle) {
                    break;
                }
            }
            last_text
        })
    }

    fn last_repl_startup_dialog_text<S>(
        snapshot: ReplStartupDialogSnapshot,
        events: S,
        timeout_ms: u64,
        max_frames: usize,
    ) -> String
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        let canvases =
            collect_repl_startup_dialog_canvases(snapshot, events, timeout_ms, max_frames);
        let last = canvases
            .last()
            .expect("mock render should produce a canvas");
        canvas_lines(last).join("\n")
    }

    fn last_repl_status_notice_text<S>(
        context: StatusNoticeContext,
        events: S,
        timeout_ms: u64,
        max_frames: usize,
    ) -> String
    where
        S: Stream<Item = TerminalEvent> + Send + 'static,
    {
        let canvases = collect_repl_status_notice_canvases(context, events, timeout_ms, max_frames);
        let last = canvases
            .last()
            .expect("mock render should produce a canvas");
        canvas_lines(last).join("\n")
    }

    fn startup_dialog_snapshot(remote: bool, desktop: bool) -> ReplStartupDialogSnapshot {
        ReplStartupDialogSnapshot {
            remote_callout: remote.then_some(RemoteCalloutSnapshot {
                remote_dialog_seen: false,
                bridge_enabled: true,
                has_claude_ai_access_token: true,
            }),
            desktop_upsell: desktop.then_some(DesktopUpsellStartupSnapshot {
                platform: "darwin".to_string(),
                arch: "arm64".to_string(),
                config: crate::components::desktop_upsell::DesktopUpsellConfig {
                    enable_shortcut_tip: false,
                    enable_startup_dialog: true,
                },
                desktop_upsell_dismissed: false,
                desktop_upsell_seen_count: 0,
            }),
            desktop_handoff_state: DesktopHandoffState::Checking,
            desktop_handoff_error: None,
            desktop_handoff_download_message: None,
        }
    }

    #[test]
    fn command_keybinding_executes_slash_command_without_clearing_prompt() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let mut events = "draft"
            .chars()
            .map(|character| (key(KeyCode::Char(character)), 5))
            .collect::<Vec<_>>();
        let mut command_key = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('y'));
        command_key.modifiers = KeyModifiers::CONTROL;
        events.push((TerminalEvent::Key(command_key), 50));
        events.push((key(KeyCode::Esc), 200));
        let canvases = collect_repl_command_keybinding_canvases(timed_stream(events), 500, 32);
        let rendered = canvases
            .iter()
            .map(|canvas| canvas_lines(canvas).join("\n"))
            .collect::<Vec<_>>();
        assert!(
            rendered.iter().any(|text| text.contains("For more help:")),
            "Ctrl-Y command:help binding never overrode readline yank; frames=\n{}",
            rendered.join("\n---\n")
        );
        let last = rendered.last().expect("final REPL frame");
        assert!(
            last.contains("draft"),
            "command keybinding must preserve the existing prompt; canvas=\n{last}"
        );
    }

    #[test]
    fn repl_empty_session_renders_status_notices_at_logo_header_boundary() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let context = StatusNoticeContext {
            cwd: "/repo".to_string(),
            memory_files: vec![crate::utils::status_notice_definitions::MemoryFileInfo {
                path: "/repo/CLAUDE.md".to_string(),
                content: "x".repeat(
                    crate::utils::status_notice_definitions::MAX_MEMORY_CHARACTER_COUNT + 1,
                ),
            }],
            ..StatusNoticeContext::default()
        };

        let text = last_repl_status_notice_text(
            context,
            stream::iter(Vec::<TerminalEvent>::new()),
            250,
            6,
        );

        assert!(text.contains("Cometix Code"), "canvas=\n{text}");
        assert!(
            text.contains("Large CLAUDE.md will impact performance"),
            "canvas=\n{text}"
        );
        assert!(text.contains("/memory to edit"), "canvas=\n{text}");
    }

    #[test]
    fn repl_startup_dialog_snapshot_reads_desktop_config_without_remote_autoguess() {
        let mut config = GlobalConfig::default();
        config.desktop_upsell_seen_count = Some(1);
        config.desktop_upsell_dismissed = Some(false);
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_desktop_upsell".to_string(),
            serde_json::json!({"enable_startup_dialog": true}),
        )]));

        let snapshot = ReplStartupDialogSnapshot::from_readonly_global_config(&config);
        let desktop = snapshot.desktop_upsell.expect("desktop snapshot");

        assert!(snapshot.remote_callout.is_none());
        // The seen-count/dismissed fields are ordinary user config and still
        // come from the snapshot; the upsell gate itself is inert because the
        // cached GrowthBook payload no longer feeds it.
        assert!(!desktop.config.enable_startup_dialog);
        assert_eq!(desktop.desktop_upsell_seen_count, 1);
        assert!(!desktop.desktop_upsell_dismissed);
    }

    #[test]
    fn repl_exit_command_exits_without_rendering_goodbye_into_live_stream() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let text = last_repl_text(stream::iter(text_input_events("/exit")), 250, 20);

        // Maps to: CC /exit observable behavior — the goodbye goes to the
        // session write (resume replay renders `> /exit` + `⎿ Goodbye!`);
        // the live stream shows nothing before gracefulShutdown.
        assert!(
            !crate::components::exit_flow::GOODBYE_MESSAGES
                .iter()
                .any(|message| text.contains(message)),
            "goodbye must not render into the live stream:\n{text}"
        );
        assert!(
            !text.contains("would_shutdown"),
            "safe callback metadata should not leak into UI:\n{text}"
        );
    }

    #[test]
    fn repl_color_command_matches_official_async_system_completion() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        // CC color.ts:66-72 → processSlashCommand.tsx:781-798. Invalid
        // colors complete through the real deferred REPL callback without
        // storage or a model request, and leave the prompt usable.
        let text = last_repl_text_waiting_for(
            stream::iter(text_input_events("/color invalid-color")),
            "Invalid color",
            10_000,
        );
        assert!(text.contains("/color invalid-color"), "canvas=\n{text}");
        assert!(text.contains("Invalid color"), "canvas=\n{text}");
        assert!(text.contains("Available colors"), "canvas=\n{text}");
        assert!(!text.contains("Unimplemented"), "canvas=\n{text}");
    }

    #[test]
    fn color_immediate_transcript_matches_official_escaped_system_envelopes() {
        // CC REPL.tsx:4365-4380 wraps escapeXml(result), in contrast to
        // the ordinary processSlashCommand.tsx:787 raw system output.
        // This exercises the existing shared envelope carrier used by the
        // immediate branch; retained active-query timing is reviewed separately.
        let output = "Invalid color \"<blue>\". Available colors: red, default";
        let messages = local_command_result_messages(
            "color",
            "<blue>",
            &crate::utils::xml::escape_xml(output),
            false,
        );
        assert_local_command_rows(
            &messages,
            "color",
            "<blue>",
            &crate::utils::xml::escape_xml(output),
        );
        assert!(explicit_local_model_messages(&messages).is_empty());
    }

    type PluginCompletionFactory =
        Arc<dyn Fn(Option<ActiveLocalCommandUi>) -> Handler<Option<String>> + Send + Sync>;
    #[derive(Clone, Default)]
    pub(super) struct PluginCompletionObserver {
        pub(super) factory: Arc<std::sync::Mutex<Option<PluginCompletionFactory>>>,
        pub(super) active: Arc<std::sync::Mutex<Option<State<Option<ActiveLocalCommandUi>>>>>,
        pub(super) history: Arc<std::sync::Mutex<Option<State<Arc<Vec<HistoryEntry>>>>>>,
    }

    #[test]
    fn plugin_late_callback_clears_replacement_and_only_idle_promise_settles_once() {
        let _lock = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Immediate completion schedules a notification on the same process
        // runtime that main publishes before mounting the actual REPL.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _no_flicker = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let observer = PluginCompletionObserver::default();
        futures::executor::block_on(async {
            let mut app = element! { ContextProvider(value: Context::owned(observer.clone())) { ReplHarness } };
            // A synchronous render is a completed mount and drops hook owners.
            // Retain the actual live render loop while invoking captured handlers.
            let mut frames = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::pending::<TerminalEvent>())
                        .with_size(100, 30),
                ),
            );
            frames.next().await.expect("live REPL mount");
            let factory = observer
                .factory
                .lock()
                .unwrap()
                .clone()
                .expect("actual REPL callback factory");
            let mut current = observer.active.lock().unwrap().unwrap();
            let history = observer.history.lock().unwrap().unwrap();
            for busy in [false, true] {
                let captured = ActiveLocalCommandUi::from_slash_command(
                    LocalCommandUi::PluginSettings {
                        data: crate::commands::plugin::plugin::call(Some("validate old")),
                        preceding_input_blocks: Vec::new(),
                    },
                    SlashCommandInvocation::new("plugin", "validate old"),
                    busy,
                );
                let callback = factory(Some(captured));
                let replacement = || {
                    ActiveLocalCommandUi::from_slash_command(
                        LocalCommandUi::Help,
                        SlashCommandInvocation::new("help", ""),
                        busy,
                    )
                };
                current.set(Some(replacement()));
                let before = history.read().len();
                callback(Some("first <result>".into()));
                assert!(
                    current.read().is_none(),
                    "source late callback must clear the newer help UI"
                );
                let after_first = history.read().len();
                assert!(after_first > before);
                current.set(Some(replacement()));
                callback(Some("second <result>".into()));
                if busy {
                    assert!(
                        current.read().is_none(),
                        "immediate onDone has no Promise once guard"
                    );
                    assert!(history.read().len() > after_first);
                } else {
                    assert!(
                        current.read().is_some(),
                        "ordinary Promise ignores subsequent resolutions"
                    );
                    assert_eq!(history.read().len(), after_first);
                }
                current.set(None);
            }
        });
    }

    #[test]
    fn plugin_immediate_execution_tracks_query_activity_separately_from_layout() {
        for busy in [false, true] {
            let active = ActiveLocalCommandUi::from_slash_command(
                LocalCommandUi::PluginSettings {
                    data: crate::commands::plugin::plugin::call(Some("validate")),
                    preceding_input_blocks: Vec::new(),
                },
                SlashCommandInvocation::new("plugin", "validate"),
                busy,
            );
            assert!(
                active.is_immediate,
                "plugin always uses the immediate layout slot"
            );
            assert_eq!(active.should_hide_prompt_input, !busy);
            assert!(
                matches!(active.panel, LocalCommandPanel::PluginSettings { immediate_execution, .. } if immediate_execution == busy)
            );
        }
    }

    #[test]
    fn repl_plugin_validate_usage_runs_real_parent_and_restores_composer() {
        let _lock = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_runtime::initialize_test_process_runtime();
        let code = Arc::new(std::sync::atomic::AtomicI32::new(9));
        let observer = PluginCompletionObserver::default();
        let rendered = futures::executor::block_on(async {
            let mut app = element! {
                ContextProvider(value: Context::owned(code.clone())) {
                    ContextProvider(value: Context::owned(observer.clone())) { ReplHarness }
                }
            };
            let events =
                stream::iter(text_input_events("/plugins validate")).chain(stream::pending());
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
            let mut output = String::new();
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                let frame = crate::utils::race(frames.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(500)).await;
                    None
                })
                .await;
                if let Some(frame) = frame {
                    output = frame.to_string();
                }
                if output.contains("Usage: /plugin validate")
                    && !output.contains("Running validation...")
                {
                    break;
                }
            }
            // Inspect the raw completion through the real retained history
            // before dropping the render-loop owner. Original Markdown omits
            // HTML <path> tokens only during rendering, not in stored messages.
            let history = observer.history.lock().unwrap().unwrap();
            let raw = history
                .read()
                .iter()
                .filter_map(|entry| match entry {
                    HistoryEntry::Message(Message::User(user)) => Some(
                        user.content
                            .iter()
                            .filter_map(|block| match block {
                                crate::types::message::UserContent::Text(text) => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                })
                .find(|text| text.contains("Usage: /plugin validate"))
                .expect("actual raw default User completion");
            assert!(raw.contains("Usage: /plugin validate <path>\n\n"), "{raw}");
            assert!(raw.contains("  claude plugin validate <path>"), "{raw}");
            output
        });
        // Bun production and Rust focused output both omit the HTML token;
        // exact raw payload assertions above must retain both placeholders.
        assert!(
            rendered
                .lines()
                .any(|line| line.trim_end().ends_with("Usage: /plugin validate")),
            "{rendered}"
        );
        assert!(
            !rendered.contains("Usage: /plugin validate <path>"),
            "{rendered}"
        );
        assert!(!rendered.contains("execution is not wired"), "{rendered}");
        assert!(!rendered.contains("Running validation..."), "{rendered}");
        assert!(rendered.contains("❯"), "{rendered}");
        assert_eq!(code.load(std::sync::atomic::Ordering::SeqCst), 9);
    }

    #[test]
    fn repl_plugin_marketplace_list_and_help_use_real_completion_and_restore_composer() {
        let _lock = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_runtime::initialize_test_process_runtime();
        // Original PluginSettings.tsx:41-68 list -> default User output;
        // :846-850 help completes undefined -> NO_CONTENT_MESSAGE via the
        // ordinary processSlashCommand local-jsx Promise. Native input runs
        // through the actual descriptor, parent, REPL callback and history.
        for (command, expected) in [
            (
                "/plugins marketplace list",
                "Configured marketplaces:\n  • 2\n  • 10\n  • z\n  • a",
            ),
            ("/plugin help", crate::utils::messages::NO_CONTENT_MESSAGE),
        ] {
            let observer = PluginCompletionObserver::default();
            let code = Arc::new(std::sync::atomic::AtomicI32::new(9));
            let fixture = crate::commands::plugin::plugin_settings::MarketplaceConfigFixture(
                Arc::new(move || {
                    assert!(
                        !command.ends_with("help"),
                        "source help never loads marketplaces"
                    );
                    Box::pin(async {
                        Ok(serde_json::from_str::<serde_json::Value>(
                            r#"{"z":{},"10":{},"2":{},"a":{}}"#,
                        )
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .clone())
                    })
                }),
            );
            futures::executor::block_on(async {
                let mut app = element! {
                    ContextProvider(value: Context::owned(code.clone())) {
                        ContextProvider(value: Context::owned(fixture)) {
                            ContextProvider(value: Context::owned(observer.clone())) { ReplHarness }
                        }
                    }
                };
                let events = stream::iter(text_input_events(command)).chain(stream::pending());
                let mut frames = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(events).with_size(100, 40),
                ));
                let deadline = Instant::now() + Duration::from_secs(8);
                let mut output = String::new();
                let mut raw_output = None;
                while Instant::now() < deadline {
                    if let Some(frame) = crate::utils::race(frames.next(), async {
                        futures_timer::Delay::new(Duration::from_millis(100)).await;
                        None
                    })
                    .await
                    {
                        output = frame.to_string();
                    }
                    if let Some(history) = *observer.history.lock().unwrap() {
                        raw_output = history
                            .read()
                            .iter()
                            .filter_map(|entry| match entry {
                                HistoryEntry::Message(Message::User(user)) => Some(
                                    user.content
                                        .iter()
                                        .filter_map(|block| match block {
                                            crate::types::message::UserContent::Text(text) => {
                                                Some(text.as_str())
                                            }
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                ),
                                _ => None,
                            })
                            .find(|text| text.contains("<local-command-stdout>"));
                    }
                    if raw_output.is_some()
                        && output.contains("❯")
                        && !output.contains("Loading marketplaces...")
                        && !output.contains("Plugin Command Usage:")
                    {
                        break;
                    }
                }
                assert_eq!(
                    raw_output.as_deref(),
                    Some(
                        format!("<local-command-stdout>{expected}</local-command-stdout>").as_str()
                    ),
                    "{command}: {output}"
                );
                assert!(output.contains("❯"), "{output}");
                assert!(!output.contains("execution is not wired"), "{output}");
                assert!(!output.contains("Loading marketplaces..."), "{output}");
                assert!(
                    !output.contains("Plugin Command Usage:"),
                    "source help ends after its no-payload effect: {output}"
                );
                assert_eq!(code.load(std::sync::atomic::Ordering::SeqCst), 9);
            });
        }
    }

    #[test]
    fn plugin_validation_status_is_retained_until_explicit_ctrl_d_exit_overrides_zero() {
        // Source: ValidatePlugin.tsx stores1 before failed onComplete; its usage
        // branch never stores. REPL.handleExit -> commands/exit/exit.tsx:42
        // calls gracefulShutdown(0), overriding the process default on exit.
        let _lock = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_runtime::initialize_test_process_runtime();
        let code = Arc::new(std::sync::atomic::AtomicI32::new(9));
        futures::executor::block_on(async {
            let mut app = element! {
                ContextProvider(value: Context::owned(code.clone())) { ReplHarness }
            };
            let (keys, events) = async_channel::unbounded();
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
            frames
                .next()
                .await
                .expect("real REPL and KeybindingRuntime mounted");
            // A real existing non-JSON local file produces a validation result
            // failure; no external model, synthetic completion or service mock.
            let invalid = format!("/plugin validate {}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
            for (command, expected) in [
                (invalid.as_str(), "Validation failed"),
                ("/plugins validate", "Usage: /plugin validate"),
            ] {
                for event in text_input_events(command) {
                    keys.try_send(event).unwrap();
                }
                let deadline = Instant::now() + Duration::from_secs(8);
                let mut output = String::new();
                loop {
                    assert!(
                        Instant::now() < deadline,
                        "waiting for {expected}: {output}"
                    );
                    let frame = crate::utils::race(frames.next(), async {
                        futures_timer::Delay::new(Duration::from_millis(500)).await;
                        None
                    })
                    .await;
                    if let Some(frame) = frame {
                        output = frame.to_string();
                    }
                    if output.contains(expected) && !output.contains("Running validation...") {
                        break;
                    }
                }
                assert_eq!(
                    code.load(std::sync::atomic::Ordering::SeqCst),
                    1,
                    "validation sets1; usage must preserve1"
                );
            }
            keys.try_send(ctrl_key('d')).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                assert!(
                    Instant::now() < deadline,
                    "first Ctrl-D did not show exit hint"
                );
                let frame = crate::utils::race(frames.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if frame
                    .is_some_and(|frame| frame.to_string().contains("Press Ctrl-D again to exit"))
                {
                    break;
                }
            }
            assert_eq!(code.load(std::sync::atomic::Ordering::SeqCst), 1);
            keys.try_send(ctrl_key('d')).unwrap();
            let completed = crate::utils::race(
                async {
                    while frames.next().await.is_some() {}
                    true
                },
                async {
                    futures_timer::Delay::new(Duration::from_secs(3)).await;
                    false
                },
            )
            .await;
            assert!(completed, "second Ctrl-D must end the actual render loop");
            assert_eq!(
                code.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "explicit source gracefulShutdown(0) overrides default process status"
            );
        });
    }

    #[test]
    fn repl_context_command_completes_async_worker_into_local_command_transcript() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let text = last_repl_text_waiting_for(
            stream::iter(text_input_events("/context")),
            "Estimated usage by category",
            10_000,
        );

        assert!(text.contains("/context"), "canvas=\n{text}");
        assert!(text.contains("Context Usage"), "canvas=\n{text}");
        assert!(
            text.contains("Estimated usage by category"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored; canvas=\n{text}"
        );
    }

    #[test]
    fn repl_compact_command_has_production_owner_and_reports_empty_history() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let text = last_repl_text(stream::iter(text_input_events("/compact")), 1_000, 24);

        assert!(text.contains("/compact"), "canvas=\n{text}");
        assert!(
            text.contains("Error during compaction: No messages to compact"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should return; canvas=\n{text}"
        );
    }

    struct KeybindingsTestEnv {
        config_dir: Option<crate::utils::env_utils::EnvVarGuard>,
        write_enabled: Option<crate::utils::env_utils::EnvVarGuard>,
        root: std::path::PathBuf,
    }

    impl Drop for KeybindingsTestEnv {
        fn drop(&mut self) {
            drop(self.config_dir.take());
            drop(self.write_enabled.take());
            crate::utils::config::clear_global_config_cache_for_testing();
            crate::keybindings::load_user_bindings::reset_keybinding_loader_for_testing();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn install_keybindings_test_env(root: std::path::PathBuf) -> KeybindingsTestEnv {
        let restore = KeybindingsTestEnv {
            config_dir: Some(crate::utils::env_utils::EnvVarGuard::set(
                "CLAUDE_CONFIG_DIR",
                &root,
            )),
            write_enabled: Some(crate::utils::env_utils::EnvVarGuard::set(
                "COMETIX_WRITE_ENABLED",
                "1",
            )),
            root: root.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();
        crate::keybindings::load_user_bindings::reset_keybinding_loader_for_testing();
        restore
    }

    fn write_stats_fixture(root: &std::path::Path) {
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let now = chrono::Utc::now();
        let later = now + chrono::Duration::minutes(5);
        let rows = [
            serde_json::json!({
                "type": "user",
                "timestamp": now.to_rfc3339(),
                "message": { "role": "user", "content": "hello" }
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": later.to_rfc3339(),
                "message": {
                    "role": "assistant",
                    "model": "claude-sonnet-4-6",
                    "usage": { "input_tokens": 1200, "output_tokens": 300 },
                    "content": [{ "type": "text", "text": "hello" }]
                }
            }),
        ];
        std::fs::write(
            project.join("stats-session.jsonl"),
            format!(
                "{}\n",
                rows.iter()
                    .map(serde_json::Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();
    }

    #[test]
    fn repl_keybindings_creates_official_template_off_frame_and_restores_prompt() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-repl-keybindings-{}", uuid::Uuid::new_v4()));
        let _restore = install_keybindings_test_env(root.clone());

        let text = last_repl_text(stream::iter(text_input_events("/keybindings")), 1_500, 40);
        let path = root.join("keybindings.json");
        let template = std::fs::read_to_string(&path).expect("created keybindings template");

        let compact_text = text
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let compact_expected = format!(
            "Created {} with template. Opened in your editor.",
            path.display()
        )
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
        assert!(compact_text.contains(&compact_expected), "canvas=\n{text}");
        assert_eq!(
            template,
            crate::keybindings::template::generate_keybindings_template()
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored; canvas=\n{text}"
        );
    }

    #[test]
    fn repl_stats_loads_real_transcript_aggregates_off_frame() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-repl-stats-{}", uuid::Uuid::new_v4()));
        write_stats_fixture(&root);
        let _projects = crate::utils::session_storage::set_test_projects_dir_override(&root);
        let _cache =
            crate::utils::stats_cache::set_test_stats_cache_path(root.join("stats-cache.json"));

        let text = last_repl_text(stream::iter(text_input_events("/stats")), 1_500, 40);

        assert!(text.contains("Overview"), "canvas=\n{text}");
        assert!(text.contains("Sessions: 1"), "canvas=\n{text}");
        assert!(text.contains("Total tokens: 1.5k"), "canvas=\n{text}");
        assert!(text.contains("Esc to cancel"), "canvas=\n{text}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repl_stats_escape_appends_official_system_display_result_and_restores_prompt() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-repl-stats-{}", uuid::Uuid::new_v4()));
        write_stats_fixture(&root);
        let _projects = crate::utils::session_storage::set_test_projects_dir_override(&root);
        let _cache =
            crate::utils::stats_cache::set_test_stats_cache_path(root.join("stats-cache.json"));
        let mut events = text_input_events("/stats")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        events.push((key(KeyCode::Esc), 500));

        let text = last_repl_text(timed_stream(events), 1_500, 40);

        assert!(text.contains("/stats"), "canvas=\n{text}");
        assert!(text.contains("Stats dialog dismissed"), "canvas=\n{text}");
        assert!(
            text.contains("❯"),
            "PromptInput should be restored; canvas=\n{text}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repl_plan_enable_then_inspect_no_plan_matches_official_on_done_rows() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _plan_lock = crate::utils::plans::test_plan_state_lock();
        crate::utils::plans::clear_plan_slug(None);
        let mut events = text_input_events("/plan")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        let mut second = text_input_events("/plan")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        if let Some((_, delay)) = second.first_mut() {
            *delay = 250;
        }
        events.extend(second);

        let text = last_repl_text(timed_stream(events), 1_500, 40);
        crate::utils::plans::clear_plan_slug(None);

        assert!(text.contains("Enabled plan mode"), "canvas=\n{text}");
        assert!(
            text.contains("Already in plan mode. No plan written yet."),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored; canvas=\n{text}"
        );
    }

    #[test]
    #[ignore = "Remote Control/bridge startup is explicitly out of the current local-only scope"]
    fn repl_startup_dialogs_follow_official_remote_before_desktop_priority() {
        let text = last_repl_startup_dialog_text(
            startup_dialog_snapshot(true, true),
            stream::iter(Vec::<TerminalEvent>::new()),
            250,
            6,
        );

        assert!(text.contains("Remote Control"), "canvas=\n{text}");
        assert!(text.contains("claude.ai/"), "canvas=\n{text}");
        assert!(
            !text.contains("Try Claude Code Desktop"),
            "desktop upsell should wait behind remote callout:\n{text}"
        );
    }

    #[test]
    fn repl_startup_dialog_desktop_upsell_renders_without_remote_side_effects() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        // Remote Control/bridge is intentionally absent. Keep local Desktop
        // startup coverage independent instead of simulating a remote-dismiss
        // transition that the production snapshot cannot currently create.
        let text = last_repl_startup_dialog_text(
            startup_dialog_snapshot(false, true),
            stream::iter(Vec::<TerminalEvent>::new()),
            250,
            6,
        );

        assert!(text.contains("Try Claude Code Desktop"), "canvas=\n{text}");
        assert!(text.contains("visual diffs"), "canvas=\n{text}");
        assert!(
            !text.contains("would_mark_seen"),
            "safe callback metadata should not leak into UI:\n{text}"
        );
    }

    fn write_resume_fixture(
        project_path: &str,
        session_id: &str,
        prompt_text: &str,
        assistant_text: &str,
    ) -> PathBuf {
        write_resume_fixture_with_custom_title(
            project_path,
            session_id,
            prompt_text,
            assistant_text,
            None,
        )
    }

    fn write_resume_fixture_with_custom_title(
        project_path: &str,
        session_id: &str,
        prompt_text: &str,
        assistant_text: &str,
        custom_title: Option<&str>,
    ) -> PathBuf {
        let session_file =
            crate::utils::session_storage::get_session_file_path(project_path, session_id);
        std::fs::create_dir_all(
            session_file
                .parent()
                .expect("session fixture should have a parent directory"),
        )
        .expect("session fixture directory should be created");
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let mut fixture_entries = vec![
            json!({
                "type": "user",
                "uuid": user_uuid,
                "sessionId": session_id,
                "cwd": project_path,
                "message": {"role": "user", "content": prompt_text}
            }),
            json!({
                "type": "assistant",
                "uuid": assistant_uuid,
                "parentUuid": user_uuid,
                "sessionId": session_id,
                "cwd": project_path,
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": assistant_text}]
                }
            }),
        ];
        if let Some(custom_title) = custom_title {
            fixture_entries.push(json!({
                "type": "custom-title",
                "sessionId": session_id,
                "customTitle": custom_title
            }));
        }
        let fixture_lines = fixture_entries
            .into_iter()
            .map(|entry| entry.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&session_file, fixture_lines)
            .expect("session fixture should be written before readonly resume");
        session_file
    }

    fn write_empty_resume_fixture(project_path: &str, session_id: &str) -> PathBuf {
        let session_file =
            crate::utils::session_storage::get_session_file_path(project_path, session_id);
        std::fs::create_dir_all(
            session_file
                .parent()
                .expect("session fixture should have a parent directory"),
        )
        .expect("session fixture directory should be created");
        std::fs::write(&session_file, "")
            .expect("empty session fixture should be written before readonly resume");
        session_file
    }

    #[test]
    fn messages_memo_key_tracks_verbose_gate() {
        let messages = Arc::new(vec![RenderableMessage::user("u1", "hello")]);
        let status_notice_context = StatusNoticeContext::default();
        let collapsed = messages_memo_key(
            &messages,
            0,
            false,
            false,
            true,
            80,
            24,
            None,
            None,
            false,
            &status_notice_context,
            None,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &[],
        );
        let expanded = messages_memo_key(
            &messages,
            0,
            false,
            true,
            true,
            80,
            24,
            None,
            None,
            false,
            &status_notice_context,
            None,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &[],
        );
        assert_ne!(
            collapsed, expanded,
            "verbose config changes must invalidate memoized transcript rows"
        );
    }

    /// Maps to: CC `Messages.tsx:1064-1065`, which compares `inProgressToolUseIDs`
    /// with `setsEqual` in the memo comparator.
    ///
    /// A tool starting or finishing changes nothing about the messages array —
    /// only the REPL-owned set — so if the key ignores it, `Messages` keeps its
    /// retained subtree and every row's `should_render_statically` decision
    /// goes stale: a finished tool would keep re-rendering dynamically and a
    /// started one would stay frozen in its static snapshot.
    #[test]
    fn messages_memo_key_tracks_in_progress_tool_use_ids() {
        let messages = Arc::new(vec![RenderableMessage::user("u1", "hello")]);
        let status_notice_context = StatusNoticeContext::default();
        let key = |live: &[&str]| {
            let live = live
                .iter()
                .map(|id| (*id).to_string())
                .collect::<std::collections::HashSet<String>>();
            messages_memo_key(
                &messages,
                0,
                false,
                false,
                true,
                80,
                24,
                None,
                None,
                false,
                &status_notice_context,
                None,
                &live,
                &std::collections::HashSet::new(),
                &[],
            )
        };

        assert_ne!(
            key(&[]),
            key(&["toolu_1"]),
            "a tool starting must invalidate"
        );
        assert_ne!(
            key(&["toolu_1"]),
            key(&[]),
            "a tool finishing must invalidate"
        );
        // Set order must not leak into the key, or an unchanged set would
        // invalidate at random and defeat the memo entirely.
        assert_eq!(key(&["a", "b"]), key(&["b", "a"]));
    }

    /// Same hazard for the streaming set (CC `Messages.tsx:690`): a tool_use
    /// block arriving mid-stream changes neither the messages array nor the
    /// in-progress set, so only this key can invalidate the subtree.
    #[test]
    fn messages_memo_key_tracks_streaming_tool_use_ids() {
        let messages = Arc::new(vec![RenderableMessage::user("u1", "hello")]);
        let status_notice_context = StatusNoticeContext::default();
        let key = |streaming: &[&str]| {
            let streaming = streaming
                .iter()
                .map(|id| (*id).to_string())
                .collect::<std::collections::HashSet<String>>();
            messages_memo_key(
                &messages,
                0,
                false,
                false,
                true,
                80,
                24,
                None,
                None,
                false,
                &status_notice_context,
                None,
                &std::collections::HashSet::new(),
                &streaming,
                &[],
            )
        };

        assert_ne!(key(&[]), key(&["toolu_1"]));
        assert_eq!(key(&["a", "b"]), key(&["b", "a"]));
    }

    /// Maps to: CC `Messages.tsx:1079-1087` — the memo comparator's `tools`
    /// case. A rebuilt pool with the same NAMES is equal (CC's `p.every((tool,
    /// i) => tool.name === n[i]?.name)`), so the per-turn pool this port
    /// assembles does not bust the subtree every frame; a pool that lost a tool
    /// is not, so the rows re-render and a narrowed-out tool stops rendering as
    /// though it were still available (`AgentTool/UI.tsx:1096-1098`).
    #[test]
    fn messages_memo_key_tracks_the_tool_pool_by_name() {
        let messages = Arc::new(vec![RenderableMessage::user("u1", "hello")]);
        let status_notice_context = StatusNoticeContext::default();
        let key = |names: &[&str]| {
            let tools = names
                .iter()
                .map(|name| crate::types::tools::Tool {
                    name: (*name).to_string(),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            messages_memo_key(
                &messages,
                0,
                false,
                false,
                true,
                80,
                24,
                None,
                None,
                false,
                &status_notice_context,
                None,
                &std::collections::HashSet::new(),
                &std::collections::HashSet::new(),
                &tools,
            )
        };

        assert_eq!(key(&["Bash", "Read"]), key(&["Bash", "Read"]));
        assert_ne!(key(&["Bash", "Read"]), key(&["Bash"]));
        // CC compares position-by-position, so order is part of the identity.
        assert_ne!(key(&["Bash", "Read"]), key(&["Read", "Bash"]));
    }

    #[test]
    fn streaming_text_line_mode_matches_official_completed_line_preview() {
        assert_eq!(
            visible_streaming_text("partial", StreamingTextDisplayMode::Line),
            None
        );
        assert_eq!(
            visible_streaming_text("hello\npartial", StreamingTextDisplayMode::Line),
            Some("hello\n".to_string())
        );
    }

    #[test]
    fn streaming_text_character_mode_preserves_cometix_incremental_preview() {
        assert_eq!(
            visible_streaming_text("partial", StreamingTextDisplayMode::Character),
            Some("partial".to_string())
        );
        assert_eq!(
            visible_streaming_text("hello\npartial", StreamingTextDisplayMode::Character),
            Some("hello\npartial".to_string())
        );
    }

    fn spinner_input() -> ShowSpinnerInput {
        ShowSpinnerInput {
            tool_jsx_allows_spinner: true,
            tool_use_confirm_queue_empty: true,
            prompt_queue_empty: true,
            ..Default::default()
        }
    }

    #[test]
    fn show_spinner_matches_official_activity_and_suppression_matrix() {
        let mut input = spinner_input();
        assert!(!should_show_spinner(input));

        input.is_loading = true;
        assert!(should_show_spinner(input));
        input.is_loading = false;
        input.user_input_on_processing = true;
        assert!(should_show_spinner(input));
        input.user_input_on_processing = false;
        input.has_running_teammates = true;
        assert!(should_show_spinner(input));
        input.has_running_teammates = false;
        input.command_queue_len = 1;
        assert!(should_show_spinner(input));

        for suppress in [
            |value: &mut ShowSpinnerInput| value.tool_jsx_allows_spinner = false,
            |value: &mut ShowSpinnerInput| value.tool_use_confirm_queue_empty = false,
            |value: &mut ShowSpinnerInput| value.prompt_queue_empty = false,
            |value: &mut ShowSpinnerInput| value.pending_worker_request = true,
            |value: &mut ShowSpinnerInput| value.only_sleep_tool_active = true,
        ] {
            let mut suppressed = spinner_input();
            suppressed.is_loading = true;
            suppress(&mut suppressed);
            assert!(!should_show_spinner(suppressed));
        }

        let mut streaming = spinner_input();
        streaming.is_loading = true;
        streaming.visible_streaming_text = true;
        assert!(!should_show_spinner(streaming));
        streaming.is_brief_only = true;
        assert!(should_show_spinner(streaming));
    }

    #[test]
    fn only_sleep_tool_active_uses_last_typed_assistant_and_in_progress_set() {
        let typed = vec![Message::Assistant(
            crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_sleep".to_string()),
                        name: "Sleep".to_string(),
                        input: serde_json::json!({"duration": 1}),
                    },
                )],
                model: None,
                stop_reason: Some(crate::types::message::StopReason::ToolUse),
                usage: None,
            },
        )];
        let ids = |live: &[&str]| {
            live.iter()
                .map(|id| (*id).to_string())
                .collect::<std::collections::HashSet<String>>()
        };

        // Live in the REPL-owned set → the turn is a pure Sleep wait.
        assert!(only_sleep_tool_active(&typed, &ids(&["toolu_sleep"])));
        // Resolved, so the actor removed it from the set: no live tool at all.
        assert!(!only_sleep_tool_active(&typed, &ids(&[])));
        // A live non-Sleep tool disqualifies the turn even with Sleep present.
        let mut mixed = typed.clone();
        if let Message::Assistant(assistant) = &mut mixed[0] {
            assistant
                .content
                .push(crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_bash".to_string()),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "ls"}),
                    },
                ));
        }
        assert!(!only_sleep_tool_active(
            &mixed,
            &ids(&["toolu_sleep", "toolu_bash"])
        ));
    }

    #[test]
    fn repl_show_spinner_stays_visible_for_queued_task_notifications() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();
        let mut notification = crate::utils::message_queue_manager::QueuedCommand::new(
            "<task-notification>",
            "task-notification",
        );
        notification.is_meta = true;
        crate::utils::message_queue_manager::enqueue_pending_notification(notification);

        let canvases =
            collect_repl_spinner_owner_canvases(stream::empty::<TerminalEvent>(), 150, 8);
        crate::utils::message_queue_manager::clear_command_queue();
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|text| text.contains("Repl-owned-spinner…")),
            "queued notifications should retain the REPL spinner; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
    }

    #[test]
    fn repl_show_spinner_projects_running_teammate_idle_leader_state() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let canvases =
            collect_repl_running_teammate_canvases(stream::empty::<TerminalEvent>(), 150, 8);
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|text| text.contains("Idle · teammates running")),
            "running teammates should retain SpinnerWithVerb while the leader is idle; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
    }

    #[test]
    fn stop_hook_spinner_suffix_matches_official_default_and_custom_forms() {
        let mut state = StopHookSpinnerState::default();
        assert_eq!(stop_hook_spinner_suffix(&state, true), None);

        state.apply(StopHookProgressEvent::Started {
            tool_use_id: "hooks-1".to_string(),
            hook_event: "Stop".to_string(),
            command: "echo one".to_string(),
            status_message: None,
            total: 2,
        });
        assert_eq!(
            stop_hook_spinner_suffix(&state, true).as_deref(),
            Some("running stop hooks… 0/2")
        );
        state.apply(StopHookProgressEvent::Completed {
            tool_use_id: "hooks-1".to_string(),
            hook_event: "Stop".to_string(),
        });
        assert_eq!(
            stop_hook_spinner_suffix(&state, true).as_deref(),
            Some("running stop hooks… 1/2")
        );

        let mut custom = StopHookSpinnerState::default();
        custom.apply(StopHookProgressEvent::Started {
            tool_use_id: "hooks-2".to_string(),
            hook_event: "Stop".to_string(),
            command: "echo custom".to_string(),
            status_message: Some("Cleaning up".to_string()),
            total: 1,
        });
        assert_eq!(
            stop_hook_spinner_suffix(&custom, true).as_deref(),
            Some("Cleaning up…")
        );
        assert_eq!(stop_hook_spinner_suffix(&custom, false), None);
    }

    #[test]
    fn resize_clear_matches_official_width_and_height_shrink_behavior() {
        assert!(resize_requires_clear_terminal((100, 35), (80, 35)));
        assert!(resize_requires_clear_terminal((100, 35), (120, 35)));
        assert!(resize_requires_clear_terminal((100, 35), (100, 20)));
        assert!(!resize_requires_clear_terminal((100, 35), (100, 40)));
        assert!(!resize_requires_clear_terminal((0, 35), (80, 35)));
    }

    #[test]
    fn logo_header_is_not_duplicated_across_main_screen_states() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _lock = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "cometix-repl-logo-header-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_home = root.join("config");
        std::fs::create_dir_all(&config_home).unwrap();
        let _cwd_guard = OriginalCwdGuard::set(&root);
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", &PathBuf::from("0"));

        let empty_text = last_repl_text(stream::empty::<TerminalEvent>(), 100, 3);
        let logo_title = "Cometix Code";
        assert_eq!(
            empty_text.matches(logo_title).count(),
            1,
            "empty main screen should render one LogoHeader-equivalent; canvas=\n{empty_text}"
        );

        let local_command_text =
            last_repl_text(stream::iter(text_input_events("/status")), 100, 20);
        assert!(
            local_command_text.contains("Settings") && local_command_text.contains("Status"),
            "status local command UI should be active; canvas=\n{local_command_text}"
        );
        assert_eq!(
            local_command_text.matches(logo_title).count(),
            1,
            "local command UI should preserve exactly one logo context; canvas=\n{local_command_text}"
        );

        let pending_query_text =
            last_repl_text(stream::iter(text_input_events("hello logo")), 250, 20);
        assert!(
            pending_query_text.contains("hello logo"),
            "pending/non-empty transcript should render user context; canvas=\n{pending_query_text}"
        );
        assert_eq!(
            pending_query_text.matches(logo_title).count(),
            1,
            "non-empty transcript should have one transcript-owned LogoHeader-equivalent; canvas=\n{pending_query_text}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restored_session_transcript_owns_single_logo_header() {
        let canvases = futures::executor::block_on(
            element!(RestoredSessionLogoProbe)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(100, 35))
                .collect::<Vec<_>>(),
        );
        let text = canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a canvas"),
        )
        .join("\n");

        assert!(
            text.contains("restored session row"),
            "restored transcript row should render; canvas=\n{text}"
        );
        let logo_title = "Cometix Code";
        assert_eq!(
            text.matches(logo_title).count(),
            1,
            "restored/non-empty transcript should own exactly one logo header; canvas=\n{text}"
        );
    }

    #[test]
    fn processed_resume_matches_official_initial_props_and_outer_restore_stores() {
        let target = resume::ResumeTarget {
            session_id: "session-1".to_string(),
            project_path: Some("/tmp/project".to_string()),
            entries: vec![json!({
                "type": "user",
                "uuid": "user-1",
                "message": {"content": "restored prompt"}
            })],
            turn_interruption_state:
                crate::utils::conversation::TurnInterruptionState::InterruptedPrompt,
            metadata: resume::ResumeMetadata {
                file_history_snapshots: vec![json!({
                    "messageId": "user-1",
                    "trackedFileBackups": {
                        "/tmp/project/src/lib.rs": {
                            "backupFileName": "src-lib-v1",
                            "version": 1,
                            "backupTime": "2026-06-29T00:00:00.000Z"
                        }
                    },
                    "timestamp": "2026-06-29T00:00:00.000Z"
                })],
                attribution_snapshots: vec![json!({"type": "attribution_snapshot"})],
                content_replacements: vec![json!({
                    "kind": "tool-result",
                    "toolUseId": "toolu_replace",
                    "replacement": "stored preview"
                })],
                context_collapse_commits: vec![json!({"type": "context_collapse_commit"})],
                context_collapse_snapshot: Some(json!({"type": "context_collapse_snapshot"})),
                read_file_state: vec![crate::utils::query_helpers::ReadFileStateEntry {
                    path: "/tmp/project/src/lib.rs".to_string(),
                    content: Some("fn main() {}".to_string()),
                    timestamp_ms: Some(1),
                    offset: None,
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Read,
                }],
                todos: vec![
                    json!({"content": "resume", "status": "pending", "activeForm": "Resuming"}),
                ],
                agent_name: Some("worker".to_string()),
                agent_color: Some("green".to_string()),
                agent_setting: Some("reviewer".to_string()),
                custom_title: Some("Saved title".to_string()),
                tag: Some("demo".to_string()),
                mode: Some("normal".to_string()),
                bash_tools: vec!["git".to_string()],
                ..resume::ResumeMetadata::default()
            },
            entrypoint: Some(ResumeEntrypoint::SlashCommandPicker),
        };

        let loaded = crate::utils::session_restore::ResumeLoadResult::try_from(&target)
            .expect("target should load");
        let result = crate::utils::session_restore::process_resumed_conversation(
            loaded,
            None,
            Arc::new(crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default()),
        )
        .expect("resume should process");
        let stores = result.resume_restore_stores.as_ref();

        assert_eq!(stores.session_id.as_deref(), Some("session-1"));
        assert_eq!(stores.project_path.as_deref(), Some("/tmp/project"));
        assert_eq!(
            stores.entrypoint,
            Some(ResumeEntrypoint::SlashCommandPicker)
        );
        assert_eq!(
            stores.turn_interruption_state,
            Some(crate::utils::conversation::TurnInterruptionState::InterruptedPrompt)
        );
        assert_eq!(stores.custom_title.as_deref(), Some("Saved title"));
        assert_eq!(stores.tag.as_deref(), Some("demo"));
        assert_eq!(stores.session_id.as_deref(), Some("session-1"));
        assert_eq!(stores.read_file_state.len(), 1);
        assert_eq!(stores.bash_tools, vec!["git".to_string()]);
        assert_eq!(stores.attribution_snapshots.len(), 1);
        assert_eq!(stores.content_replacements.len(), 1);
        assert_eq!(stores.context_collapse.commits.len(), 1);
        assert!(stores.context_collapse.snapshot.is_some());
        assert_eq!(
            stores
                .file_history
                .as_ref()
                .expect("file history should restore")
                .tracked_files,
            vec!["src/lib.rs".to_string()]
        );
        assert_eq!(
            stores
                .todos_by_session
                .get("session-1")
                .and_then(|todos| todos.first())
                .and_then(|todo| todo.get("content"))
                .and_then(serde_json::Value::as_str),
            Some("resume")
        );
        assert_eq!(
            stores
                .standalone_agent_context
                .as_ref()
                .map(|context| (context.name.as_str(), context.color.as_deref())),
            Some(("worker", Some("green")))
        );
        assert_eq!(stores.agent_setting.as_deref(), Some("reviewer"));
        assert_eq!(stores.mode.as_deref(), Some("normal"));
        assert!(matches!(
            &result.renderable_messages[0].kind,
            RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(crate::types::message::UserContent::Text(text)) if text == "restored prompt"
            )
        ));
        assert!(matches!(
            &result.messages[0],
            Message::User(user)
                if matches!(user.content.first(), Some(crate::types::message::UserContent::Text(text)) if text == "restored prompt")
        ));
    }

    #[test]
    fn resume_load_result_rejects_empty_model_conversation() {
        let target = resume::ResumeTarget {
            session_id: "empty-session".to_string(),
            project_path: None,
            entries: vec![json!({"type": "unknown"})],
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: resume::ResumeMetadata::default(),
            entrypoint: Some(ResumeEntrypoint::SlashCommandSessionId),
        };

        let error = crate::utils::session_restore::ResumeLoadResult::try_from(&target)
            .expect_err("empty transcript should fail");
        assert_eq!(
            error,
            "Session empty-session has no renderable transcript messages."
        );
    }

    #[test]
    fn model_immediate_gate_matches_internal_or_hardcoded_feature_switch() {
        use crate::utils::build_profile::BuildAudience;
        use crate::utils::immediate_command::should_inference_config_command_be_immediate_from;

        assert!(should_inference_config_command_be_immediate_from(
            BuildAudience::AnthropicInternal,
            false
        ));
        assert!(should_inference_config_command_be_immediate_from(
            BuildAudience::External,
            true
        ));
        assert!(!should_inference_config_command_be_immediate_from(
            BuildAudience::External,
            false
        ));
        assert!(model::env_flag_truthy(Some("1")));
        assert!(model::env_flag_truthy(Some("true")));
        assert!(model::env_flag_truthy(Some("ON")));
        assert!(model::env_flag_truthy(Some(" Yes ")));
        assert!(!model::env_flag_truthy(Some("0")));
    }

    #[test]
    fn active_local_command_ui_metadata_matches_official_tooljsx_semantics() {
        let status_idle = ActiveLocalCommandUi::from_slash_command(
            LocalCommandUi::Settings {
                default_tab: LocalSettingsTab::Status,
            },
            slash_invocation("status", ""),
            false,
        );
        assert!(status_idle.is_local_command_ui);
        assert!(status_idle.is_immediate);
        assert!(status_idle.should_hide_prompt_input);
        assert_eq!(
            status_idle
                .invocation
                .as_ref()
                .map(|inv| inv.slash_command.command_name.as_str()),
            Some("status")
        );

        let status_during_query = ActiveLocalCommandUi::from_slash_command(
            LocalCommandUi::Settings {
                default_tab: LocalSettingsTab::Status,
            },
            slash_invocation("status", ""),
            true,
        );
        assert!(status_during_query.is_immediate);
        assert!(
            !status_during_query.should_hide_prompt_input,
            "official immediate local command UI path keeps PromptInput mounted while a query is active"
        );

        let hooks_during_query = ActiveLocalCommandUi::from_slash_command(
            LocalCommandUi::Hooks,
            slash_invocation("hooks", ""),
            true,
        );
        assert!(hooks_during_query.is_immediate);
        assert!(!hooks_during_query.should_hide_prompt_input);

        let config_panel = ActiveLocalCommandUi::from_slash_command(
            LocalCommandUi::Settings {
                default_tab: LocalSettingsTab::Config,
            },
            slash_invocation("config", ""),
            true,
        );
        assert!(!config_panel.is_immediate);
        assert!(config_panel.should_hide_prompt_input);

        let help_keybinding_during_query = ActiveLocalCommandUi::from_slash_command_with_source(
            LocalCommandUi::Help,
            slash_invocation("help", ""),
            true,
            true,
        );
        assert!(help_keybinding_during_query.is_immediate);
        assert!(
            !help_keybinding_during_query.should_hide_prompt_input,
            "fromKeybinding makes any local-jsx command immediate during an active query"
        );
    }

    #[test]
    fn local_command_ui_state_preserves_official_tooljsx_clear_and_overwrite_semantics() {
        let active = ActiveLocalCommandUi::from_slash_command(
            LocalCommandUi::Settings {
                default_tab: LocalSettingsTab::Status,
            },
            slash_invocation("status", ""),
            true,
        );
        let preserved =
            preserve_local_command_ui_against_non_local_tool_update(Some(active.clone()));
        assert_eq!(preserved, Some(active.clone()));

        assert_eq!(
            clear_local_command_ui(Some(active.clone())),
            None,
            "explicit clearLocalJSX equivalent should clear the preserved local command UI"
        );

        let non_local_tool_ui = ActiveLocalCommandUi {
            is_local_command_ui: false,
            should_hide_prompt_input: false,
            is_immediate: false,
            ..active.clone()
        };
        assert_eq!(
            clear_local_command_ui(Some(non_local_tool_ui.clone())),
            Some(non_local_tool_ui.clone()),
            "clearLocalJSX equivalent should not clear ordinary non-local tool UI state"
        );
        assert_eq!(
            set_local_command_ui(Some(active.clone()), non_local_tool_ui),
            Some(active.clone()),
            "ordinary non-local tool UI updates should not overwrite an active local command UI"
        );

        let replacement = ActiveLocalCommandUi::from_slash_command(
            LocalCommandUi::Help,
            slash_invocation("help", ""),
            false,
        );
        assert_eq!(
            set_local_command_ui(Some(active), replacement.clone()),
            Some(replacement),
            "new local command UI descriptors replace the currently active one"
        );
    }

    /// Local command rows are `System(LocalCommand)` (CC
    /// `createCommandInputMessage` breadcrumb + tagged stdout). The shared
    /// history persists them; API normalization converts them to User text.
    fn assert_local_command_rows(
        messages: &[RenderableMessage],
        command: &str,
        args: &str,
        output: &str,
    ) {
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            &messages[0].kind,
            RenderableMessageKind::System(SystemMessage::LocalCommand { content, .. })
                if content.contains(&format!("<command-name>/{command}</command-name>"))
                    && content.contains(&format!("<command-args>{args}</command-args>"))
        ));
        assert!(matches!(
            &messages[1].kind,
            RenderableMessageKind::System(SystemMessage::LocalCommand { content, .. })
                if content == &format!("<local-command-stdout>{output}</local-command-stdout>")
        ));
    }

    #[test]
    fn resume_by_arg_error_messages_match_official_on_done_shape() {
        let invocation = local_invocation("resume", "missing-session");
        let messages = local_command_invocation_result_messages(
            &invocation,
            "Session missing-session was not found.",
            false,
        );

        assert_local_command_rows(
            &messages,
            "resume",
            "missing-session",
            "Session missing-session was not found.",
        );
    }

    #[test]
    fn local_command_ui_dismiss_messages_append_command_and_output_rows() {
        let active = ActiveLocalCommandUi::from_slash_command(
            LocalCommandUi::Settings {
                default_tab: LocalSettingsTab::Status,
            },
            slash_invocation("status", ""),
            false,
        );
        let messages = local_command_ui_dismiss_messages(&active);

        assert_local_command_rows(&messages, "status", "", "Status dialog dismissed");
    }

    #[test]
    fn config_local_command_ui_dismiss_messages_use_official_config_string() {
        let active = ActiveLocalCommandUi::from_slash_command(
            LocalCommandUi::Settings {
                default_tab: LocalSettingsTab::Config,
            },
            slash_invocation("config", ""),
            false,
        );
        let messages = local_command_ui_dismiss_messages(&active);

        assert_local_command_rows(&messages, "config", "", "Config dialog dismissed");
    }

    #[test]
    fn command_panel_dismiss_strings_match_official_on_done_outputs() {
        let cases = [
            (LocalCommandUi::Help, "help", "Help dialog dismissed"),
            (LocalCommandUi::Hooks, "hooks", "Hooks dialog dismissed"),
            (
                LocalCommandUi::Model,
                "model",
                "Kept model as Sonnet 4.6 (default)",
            ),
            (
                LocalCommandUi::Effort {
                    args: None,
                    has_conversation_messages: false,
                },
                "effort",
                "Cancelled",
            ),
            (
                LocalCommandUi::Permissions,
                "permissions",
                "Permissions dialog dismissed",
            ),
            (LocalCommandUi::Memory, "memory", "Cancelled memory editing"),
            (
                LocalCommandUi::Doctor,
                "doctor",
                "Claude Code diagnostics dismissed",
            ),
            (LocalCommandUi::Diff, "diff", "Diff dialog dismissed"),
            (
                LocalCommandUi::Sandbox,
                "sandbox",
                "Sandbox dialog dismissed",
            ),
            (
                LocalCommandUi::Mcp {
                    args: String::new(),
                },
                "mcp",
                "MCP dialog dismissed",
            ),
        ];

        for (command_ui, expected_command, expected_output) in cases {
            let active = ActiveLocalCommandUi::from_slash_command(
                command_ui,
                slash_invocation(expected_command, ""),
                false,
            );
            let messages = local_command_ui_dismiss_messages(&active);

            assert_local_command_rows(&messages, expected_command, "", expected_output);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn repl_accepts_completed_speculation_without_starting_duplicate_query() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _lock = env_lock().lock().unwrap();
        let _write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", &PathBuf::from("0"));
        let events = timed_stream(vec![
            (key(KeyCode::Enter), 30),
            (TerminalEvent::Resize(100, 30), 100),
        ]);
        let mut app = element!(ReplCompletedSpeculationHarness);
        let mut render_loop =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 30),
            ));
        let mut frames = Vec::new();
        loop {
            let next = crate::utils::race(render_loop.next(), async {
                tokio::time::sleep(Duration::from_millis(150)).await;
                None
            })
            .await;
            let Some(canvas) = next else {
                break;
            };
            frames.push(canvas);
            if frames.len() >= 24 {
                break;
            }
        }
        let rendered = frames
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|text| text.contains("continue the work")),
            "accepted user prompt should render immediately"
        );
        assert!(
            rendered
                .iter()
                .any(|text| text.contains("already speculated reply")),
            "completed speculative response should be injected"
        );
        assert!(
            rendered
                .iter()
                .any(|text| text.contains("run the final checks")),
            "completed speculation should promote the pipelined suggestion"
        );
        assert!(
            rendered.iter().all(|text| !text.contains("Working…")),
            "completed speculation must not start a duplicate ordinary query"
        );
        assert!(
            frames.len() < 24,
            "mock render loop must settle within its bound"
        );
    }

    #[test]
    fn local_command_ui_escape_restores_prompt_and_appends_native_scrollback_output() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _lock = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "cometix-repl-status-panel-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_home = root.join("config");
        std::fs::create_dir_all(&config_home).unwrap();
        let _cwd_guard = OriginalCwdGuard::set(&root);
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", &PathBuf::from("0"));

        let mut events = text_input_events("/status")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        // 300ms gap: under a saturated suite the render loop lags the event
        // clock — Esc landing before the panel mounts means the dismissal
        // output never appears (same class as the permissions tests).
        events.push((key(KeyCode::Esc), 300));
        let event_stream = stream::unfold(events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        });
        let canvases = futures::executor::block_on(async {
            let mut app = element!(ReplHarness);
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(MockTerminalConfig::with_events(event_stream)),
            );
            let mut canvases = Vec::new();
            // Explicit settle: collect until the dismissal output lands (the
            // terminal state this test asserts) instead of a silence window —
            // the silence-settled collector truncates under load.
            let deadline = std::time::Instant::now() + Duration::from_millis(10_000);
            loop {
                let settled = canvases
                    .last()
                    .map(|canvas| canvas_lines(canvas).join("\n"))
                    .is_some_and(|text| text.contains("Status dialog dismissed"));
                if settled || std::time::Instant::now() >= deadline {
                    break;
                }
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if let Some(canvas) = next {
                    canvases.push(canvas);
                }
            }
            canvases
        });
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();
        assert!(
            rendered.iter().any(|text| {
                text.contains("Settings")
                    && text.contains("Status")
                    && !text.contains("? for shortcuts")
            }),
            "ordinary local command UI should hide PromptInput while active; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );

        let last = canvases
            .last()
            .expect("mock render should produce a canvas");
        let lines = canvas_lines(last);
        let text = lines.join("\n");

        assert!(
            text.contains("/status"),
            "Esc after a local command UI panel should append the command row; canvas=\n{text}"
        );
        assert!(
            text.contains("Status dialog dismissed"),
            "Esc after a local command UI panel should append the dismissal output; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after local command UI close; canvas=\n{text}"
        );
        assert!(
            !text.contains("Settings  Status"),
            "local command UI panel should be gone after Esc; canvas=\n{text}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn config_save_enter_appends_change_summary_and_restores_prompt_without_writes() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let mut timed_events = text_input_events("/config")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        // 300ms gaps + explicit settle, same as the sibling /model and
        // workspace tests. The 50ms clock let Enter land before the panel
        // had rendered, and `last_repl_text`'s silence window treated a gap
        // mid-turn as settled — under a saturated suite the assertion got the
        // still-open panel instead of the post-save frame.
        timed_events.push((key(KeyCode::Enter), 300));
        timed_events.push((key(KeyCode::Char(' ')), 300));
        timed_events.push((key(KeyCode::Enter), 300));
        let event_stream = stream::unfold(timed_events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        });
        // The readonly note only appears on the post-save summary, so it is the
        // frame the assertions want.
        let text = last_repl_text_waiting_for(
            event_stream,
            "UI-only preview; settings were not written",
            10_000,
        );

        assert!(
            text.contains("/config"),
            "config save should append the local command row; canvas=\n{text}"
        );
        assert!(
            text.contains("Disabled auto-compact"),
            "config save should append an official-shaped change summary; canvas=\n{text}"
        );
        assert!(
            text.contains("UI-only preview; settings were not written"),
            "config save summary should preserve readonly/no-write semantics; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after config save; canvas=\n{text}"
        );
        assert!(
            !text.contains("Settings  Status"),
            "config panel should be cleared after save; canvas=\n{text}"
        );
    }

    #[test]
    fn prompt_bash_mode_executes_explicit_user_command_without_model_query() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let mut events = "!printf shell-mode-ok"
            .chars()
            .map(|ch| (key(KeyCode::Char(ch)), 10))
            .collect::<Vec<_>>();
        events.push((key(KeyCode::Enter), 20));
        events.push((key(KeyCode::F(24)), 200));
        let text = last_repl_text(timed_stream(events), 500, 80);
        assert!(text.contains("! printf shell-mode-ok"), "canvas=\n{text}");
        assert!(text.contains("shell-mode-ok"), "canvas=\n{text}");
        assert!(!text.contains("Responding"), "canvas=\n{text}");
    }

    #[test]
    fn model_command_selection_appends_local_command_output_and_restores_prompt() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let mut timed_events = text_input_events("/model")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        // 300ms gaps + explicit settle: same load-sensitivity class as the
        // permissions/workspace tests (silence windows and tight event clocks
        // both truncate under a saturated suite).
        timed_events.push((key(KeyCode::Down), 300));
        timed_events.push((key(KeyCode::Enter), 300));
        let event_stream = stream::unfold(timed_events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        });
        let text = last_repl_text_waiting_for(event_stream, "Set model to", 10_000);

        assert!(
            text.contains("/model"),
            "model selection should append the local command row; canvas=\n{text}"
        );
        assert!(
            text.contains("Set model to Sonnet 4.6"),
            "model selection should append the official local command output row; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after model selection; canvas=\n{text}"
        );
        assert!(
            !text.contains("Select model"),
            "model picker should be cleared after selection; canvas=\n{text}"
        );
    }

    #[test]
    fn permissions_recent_empty_content_keeps_tab_navigation_live() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let mut timed_events = text_input_events("/permissions")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        // Initial tab is Allow when the recent-denial snapshot is empty.
        timed_events.push((key(KeyCode::Left), 50));
        timed_events.push((key(KeyCode::Down), 50));
        // CC Tabs `navFromContent` keeps tab navigation active after focus
        // enters the static empty RecentDenialsTab branch.
        timed_events.push((key(KeyCode::Right), 50));
        let text = last_repl_text_waiting_for(
            timed_stream(timed_events),
            "Claude Code won't ask before using allowed tools.",
            10_000,
        );

        assert!(
            text.contains("Claude Code won't ask before using allowed tools."),
            "Right from Recently denied content should return to Allow; canvas=\n{text}"
        );
        assert!(text.contains("Permissions:"), "canvas=\n{text}");
        assert!(
            !text.contains("No recent denials"),
            "the final selected tab should no longer be Recently denied; canvas=\n{text}"
        );
    }

    #[test]
    fn permissions_recent_empty_content_esc_restores_prompt() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        // This regression checks inline scrollback. The separate fullscreen
        // completion regression checks the source dismissal suppression branch.
        let _guard = env_lock().lock().unwrap();
        let _fullscreen = EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let mut timed_events = text_input_events("/permissions")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        timed_events.push((key(KeyCode::Left), 50));
        timed_events.push((key(KeyCode::Down), 50));
        timed_events.push((key(KeyCode::Esc), 50));
        let text = last_repl_text(timed_stream(timed_events), 100, 40);

        assert!(text.contains("/permissions"), "canvas=\n{text}");
        assert!(
            text.contains("❯"),
            "Esc from Recently denied content should restore PromptInput; canvas=\n{text}"
        );
        assert!(
            !text.contains("Permissions:"),
            "permissions panel should be cleared after Esc; canvas=\n{text}"
        );
    }

    #[test]
    fn permissions_workspace_esc_matches_official_parent_cancel_and_restores_prompt() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        // This regression checks inline scrollback. The separate fullscreen
        // completion regression checks the source dismissal suppression branch.
        let _guard = env_lock().lock().unwrap();
        let _fullscreen = EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let mut timed_events = text_input_events("/permissions")
            .into_iter()
            .map(|event| (event, 0))
            .collect::<Vec<_>>();
        // 300ms gaps: under a saturated suite the render loop lags the event
        // clock, and a navigation key landing before the panel is ready walks
        // the state machine somewhere the dismissal output never appears.
        timed_events.push((key(KeyCode::Right), 300));
        timed_events.push((key(KeyCode::Right), 300));
        timed_events.push((key(KeyCode::Right), 300));
        timed_events.push((key(KeyCode::Down), 300));
        timed_events.push((key(KeyCode::Esc), 300));
        let event_stream = stream::unfold(timed_events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        });
        let text = last_repl_text_waiting_for(event_stream, "Permissions dialog dismissed", 10_000);

        assert!(
            text.contains("/permissions"),
            "workspace close should append the local command row; canvas=\n{text}"
        );
        assert!(
            text.contains("Permissions dialog dismissed"),
            "parent confirm:no remains active after Workspace mounts and consumes Esc first; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after workspace close; canvas=\n{text}"
        );
        assert!(
            !text.contains("Permissions:"),
            "permissions panel should be cleared after workspace close; canvas=\n{text}"
        );
    }

    #[tokio::test]
    async fn permissions_workspace_add_matches_official_live_store_and_repl_completion() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        // PermissionRuleList.tsx:652-690 updates AppState, remounts Tabs at
        // defaultTab, then :543-552 emits accumulated changes on top-level Esc.
        // Drive each next key from an observed frame rather than timed guesses.
        let _guard = env_lock().lock().unwrap();
        let directory = std::env::temp_dir().join(format!("permissions-repl-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut app = element! {
            ContextProvider(value: Context::owned(
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
            )) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    IsolatedAuthRepl(app_store: Some(store.clone()))
                }
            }
        };
        let (keys, events) = async_channel::unbounded();
        for event in text_input_events("/permissions") {
            keys.try_send(event).unwrap();
        }
        let mut frames =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(140, 35),
            ));
        let deadline = futures_timer::Delay::new(Duration::from_secs(10));
        tokio::pin!(deadline);
        let mut stage = 0;
        let mut text = String::new();
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                canvas = frames.next() => {
                    let Some(canvas) = canvas else { break; };
                    text = canvas_lines(&canvas).join("\n");
                    let next = match stage {
                        0 if text.contains("Claude Code won't ask") => Some(key(KeyCode::Right)),
                        1 if text.contains("Claude Code will always ask") => Some(key(KeyCode::Right)),
                        2 if text.contains("Claude Code will always reject") => Some(key(KeyCode::Right)),
                        3 if text.contains("Original working directory") => Some(key(KeyCode::Down)),
                        4 if text.contains("❯ 1. Add directory") => Some(key(KeyCode::Enter)),
                        5 if text.contains("Enter the path to the directory:") => Some(TerminalEvent::Paste(directory.to_string_lossy().into_owned())),
                        6 if text.chars().filter(|ch| !ch.is_whitespace() && !matches!(*ch, '│' | '┃' | '║')).collect::<String>().contains(directory.file_name().unwrap().to_str().unwrap()) => Some(key(KeyCode::Enter)),
                        7 if text.contains("Claude Code won't ask") => Some(key(KeyCode::Esc)),
                        8 if text.contains("Added directory") && !text.contains("Permissions:") => { stage = 9; break; },
                        _ => None,
                    };
                    if let Some(event) = next { keys.try_send(event).unwrap(); stage += 1; }
                }
            }
        }
        assert_eq!(
            stage, 9,
            "workspace add did not complete through the actual REPL; stage={stage}; canvas=\n{text}"
        );
        let added = store
            .get()
            .tool_permission_context
            .additional_working_directories
            .values()
            .any(|entry| std::path::Path::new(&entry.path).file_name() == directory.file_name());
        assert!(added, "directory must reach the real permission context");
        assert!(
            text.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains("for this session"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "main prompt must be restored; canvas=\n{text}"
        );
        drop(frames);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn permissions_keybinding_during_active_query_matches_official_immediate_completion() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        // REPL.tsx:4302-4381: a command keybinding during an existing query
        // owns a distinct onDone path. Hold the real QueryHandle event channel
        // open, then drive the actual REPL, command dispatcher and Workspace UI.
        // No user prompt is submitted and no model actor is started.
        let _guard = env_lock().lock().unwrap();
        let _settings = crate::utils::env_utils::IsolatedProjectSettings::pin();
        for fullscreen in [false, true] {
            let _fullscreen =
                EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", if fullscreen { "1" } else { "0" });
            let directory = std::env::temp_dir()
                .join(format!("permissions-immediate-<held>&-{}", Uuid::new_v4(),));
            std::fs::create_dir_all(&directory).unwrap();
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let (query_events, query_event_rx) = async_channel::unbounded();
            let (query_commands, query_command_rx) = async_channel::unbounded();
            let abort = crate::tool::AbortController::default();
            let query_id = format!("held-permissions-query-{fullscreen}");
            let probe = ReplActiveQueryProbe {
                start_active: true,
                started_queries: Arc::new(Mutex::new(Vec::new())),
                handle: QueryHandle {
                    id: query_id.clone(),
                    events: Arc::new(query_event_rx),
                    commands: Arc::new(query_commands),
                    abort_controller: abort.clone(),
                    resume: None,
                },
                snapshot: Arc::new(Mutex::new(None)),
            };
            let mut bindings = crate::keybindings::default_bindings::default_bindings();
            bindings.push(crate::keybindings::types::ParsedBinding {
                chord: crate::keybindings::parser::parse_chord("ctrl+y"),
                action: Some("command:permissions".to_string()),
                context: crate::keybindings::types::ContextName::Chat,
            });
            let mut app = element! {
                ContextProvider(value: Context::owned(probe.clone())) {
                    ContextProvider(value: Context::owned(
                        crate::keybindings::keybinding_context::KeybindingRuntime::new(bindings)
                    )) {
                        ContextProvider(value: Context::owned(*theme::current())) {
                            IsolatedAuthRepl(app_store: Some(store.clone()))
                        }
                    }
                }
            };
            let (keys, events) = async_channel::unbounded();
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(160, 40),
            ));
            let deadline = futures_timer::Delay::new(Duration::from_secs(15));
            tokio::pin!(deadline);
            let mut stage = 0;
            let mut text = String::new();
            let mut initial_rows = 0;
            let mut initial_model_messages = Vec::new();
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    canvas = frames.next() => {
                        let Some(canvas) = canvas else { break; };
                        text = canvas_lines(&canvas).join("\n");
                        let notification_present = {
                            let state = store.get();
                            state.notifications.current.iter().chain(state.notifications.queue.iter())
                                .any(|notification| notification.key == "immediate-permissions")
                        };
                        let next = match stage {
                            0 => {
                                let snapshot = probe.snapshot.lock().unwrap();
                                let snapshot = snapshot.as_ref().unwrap();
                                assert_eq!(snapshot.query_id.as_deref(), Some(query_id.as_str()));
                                initial_rows = snapshot.rows.len();
                                initial_model_messages = snapshot.model_messages.clone();
                                Some(ctrl_key('y'))
                            },
                            1 if text.contains("Claude Code won't ask") => Some(key(KeyCode::Right)),
                            2 if text.contains("Claude Code will always ask") => Some(key(KeyCode::Right)),
                            3 if text.contains("Claude Code will always reject") => Some(key(KeyCode::Right)),
                            4 if text.contains("Original working directory") => Some(key(KeyCode::Down)),
                            5 if text.contains("❯ 1. Add directory") => Some(key(KeyCode::Enter)),
                            6 if text.contains("Enter the path to the directory:") => Some(TerminalEvent::Paste(directory.to_string_lossy().into_owned())),
                            7 if text.chars().filter(|ch| !ch.is_whitespace() && !matches!(*ch, '│' | '┃' | '║')).collect::<String>().contains(directory.file_name().unwrap().to_str().unwrap()) => Some(key(KeyCode::Enter)),
                            8 if text.contains("Claude Code won't ask") => Some(key(KeyCode::Esc)),
                            9 if notification_present && !probe.snapshot.lock().unwrap().as_ref().unwrap().local_command_open => {
                                stage = 10;
                                break;
                            },
                            _ => None,
                        };
                        if let Some(event) = next { keys.try_send(event).unwrap(); stage += 1; }
                    }
                }
            }
            assert_eq!(
                stage, 10,
                "actual REPL immediate completion stalled: fullscreen={fullscreen}, stage={stage}, canvas=\n{text}"
            );
            let state = store.get();
            let notification = state
                .notifications
                .current
                .iter()
                .chain(state.notifications.queue.iter())
                .find(|notification| notification.key == "immediate-permissions")
                .expect("immediate onDone must publish its notification");
            assert_eq!(
                notification.priority,
                crate::context::notifications::NotificationPriority::Immediate
            );
            assert!(notification.text.contains("Added directory"));
            assert!(
                notification.text.contains("<held>&"),
                "notification keeps unescaped result"
            );
            assert!(
                state
                    .tool_permission_context
                    .additional_working_directories
                    .values()
                    .any(|entry| std::path::Path::new(&entry.path).file_name()
                        == directory.file_name())
            );
            let snapshot = probe.snapshot.lock().unwrap();
            let snapshot = snapshot.as_ref().unwrap();
            assert_eq!(snapshot.query_id.as_deref(), Some(query_id.as_str()));
            // CC REPL.tsx:4365-4397 appends local_command System entries to
            // the one messages array outside fullscreen. They become User
            // only later in messages.ts:2078-2093, not at this callback.
            assert_eq!(
                &snapshot.model_messages[..initial_model_messages.len()],
                initial_model_messages.as_slice(),
            );
            let added_messages = &snapshot.model_messages[initial_model_messages.len()..];
            assert_eq!(added_messages.len(), if fullscreen { 0 } else { 2 });
            assert!(
                added_messages.iter().all(|message| matches!(
                    message,
                    Message::System(SystemMessage::LocalCommand { .. })
                )),
                "immediate completion adds no User messages or caveat"
            );
            let added_rows = &snapshot.rows[initial_rows..];
            if fullscreen {
                assert!(
                    added_rows.is_empty(),
                    "fullscreen completion uses the notification only"
                );
            } else {
                assert_eq!(added_rows.len(), 2);
                let contents = added_rows
                    .iter()
                    .map(|row| match &row.kind {
                        RenderableMessageKind::System(SystemMessage::LocalCommand {
                            content,
                            ..
                        }) => content.as_str(),
                        other => {
                            panic!("immediate output must be local_command system rows: {other:?}")
                        }
                    })
                    .collect::<Vec<_>>();
                assert!(contents[0].contains("<command-name>/permissions</command-name>"));
                assert!(
                    contents[1].contains("&lt;held&gt;&amp;"),
                    "system output must escape XML"
                );
                assert!(!contents[1].contains("<held>&"));
            }
            assert!(
                !abort.is_aborted(),
                "closing the command must not interrupt the held query"
            );
            assert!(matches!(
                query_command_rx.try_recv(),
                Err(async_channel::TryRecvError::Empty)
            ));
            drop(frames);
            drop(query_events);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn permissions_rule_input_escape_matches_official_real_repl_parent_cancel() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        // Original BaseTextInput handles Esc but does not stop the subsequent
        // PermissionRuleInput Settings binding. Exercise the real REPL command
        // mount; an isolated input lacks the other live keybinding contexts.
        let _guard = env_lock().lock().unwrap();
        let _settings = crate::utils::env_utils::IsolatedProjectSettings::pin();
        let _fullscreen = EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let mut app = element! {
            ContextProvider(value: Context::owned(
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
            )) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    IsolatedAuthRepl
                }
            }
        };
        let (keys, events) = async_channel::unbounded();
        let mut frames =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 32),
            ));
        let deadline = futures_timer::Delay::new(Duration::from_secs(10));
        tokio::pin!(deadline);
        let mut stage = 0;
        let mut text = String::new();
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                canvas = frames.next() => {
                    let Some(canvas) = canvas else { break; };
                    text = canvas_lines(&canvas).join("\n");
                    let event = match stage {
                        0 => {
                            for event in text_input_events("/permissions") { keys.try_send(event).unwrap(); }
                            stage = 1; None
                        }
                        1 if text.contains("Claude Code won't ask") => Some(key(KeyCode::Down)),
                        2 if text.contains("❯ 1. Add a new rule") => Some(key(KeyCode::Enter)),
                        3 if text.contains("Add allow permission rule") => Some(key(KeyCode::Enter)),
                        4 if text.contains("Enter permission rule") => Some(key(KeyCode::Esc)),
                        5 if text.contains("Claude Code won't ask") && !text.contains("Add allow permission rule") => Some(key(KeyCode::Esc)),
                        6 if text.contains("Permissions dialog dismissed") && !text.contains("Claude Code won't ask") => { stage = 7; break; }
                        _ => None,
                    };
                    if let Some(event) = event { keys.try_send(event).unwrap(); stage += 1; }
                }
            }
        }
        assert_eq!(
            stage, 7,
            "actual REPL input cancel stalled at stage {stage}:\n{text}"
        );
        assert!(
            !text.contains("Added allow rule"),
            "empty submit and cancel must not save"
        );
    }

    #[tokio::test]
    async fn permissions_recent_retry_matches_official_normal_and_immediate_query_paths() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        // commands/permissions/permissions.tsx: onRetryDenials precedes onDone.
        // Ordinary processUserInput resumes its hook/query continuation, while
        // REPL.tsx's command shortcut ignores shouldQuery during an active query.
        let _guard = env_lock().lock().unwrap();
        let _settings = crate::utils::env_utils::IsolatedProjectSettings::pin();
        let _fullscreen = EnvVarGuard::set("CLAUDE_CODE_NO_FLICKER", "0");
        let _title = EnvVarGuard::set("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1");
        let command = "echo permissions-retry-scout";
        crate::utils::auto_mode_denials::record_auto_mode_denial(
            crate::utils::auto_mode_denials::AutoModeDenial {
                tool_name: "Bash".into(),
                display: command.into(),
                reason: "controlled denial".into(),
                timestamp: 1.0,
            },
        );
        for start_active in [false, true] {
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let (query_events, query_event_rx) = async_channel::unbounded();
            let (query_commands, query_command_rx) = async_channel::unbounded();
            let abort = crate::tool::AbortController::default();
            let query_id = format!("permissions-retry-{start_active}");
            let probe = ReplActiveQueryProbe {
                start_active,
                started_queries: Arc::new(Mutex::new(Vec::new())),
                handle: QueryHandle {
                    id: query_id.clone(),
                    events: Arc::new(query_event_rx),
                    commands: Arc::new(query_commands),
                    abort_controller: abort.clone(),
                    resume: None,
                },
                snapshot: Arc::new(Mutex::new(None)),
            };
            let mut bindings = crate::keybindings::default_bindings::default_bindings();
            bindings.push(crate::keybindings::types::ParsedBinding {
                chord: crate::keybindings::parser::parse_chord("ctrl+y"),
                action: Some("command:permissions".into()),
                context: crate::keybindings::types::ContextName::Chat,
            });
            let mut app = element! {
                ContextProvider(value: Context::owned(probe.clone())) {
                    ContextProvider(value: Context::owned(
                        crate::keybindings::keybinding_context::KeybindingRuntime::new(bindings)
                    )) {
                        ContextProvider(value: Context::owned(*theme::current())) {
                            IsolatedAuthRepl(app_store: Some(store.clone()))
                        }
                    }
                }
            };
            let (keys, events) = async_channel::unbounded();
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(160, 40),
            ));
            let deadline = futures_timer::Delay::new(Duration::from_secs(15));
            tokio::pin!(deadline);
            let mut stage = 0;
            let mut text = String::new();
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    canvas = frames.next() => {
                        let Some(canvas) = canvas else { break; };
                        text = canvas_lines(&canvas).join("\n");
                        match stage {
                            0 => {
                                if start_active {
                                    keys.try_send(ctrl_key('y')).unwrap();
                                } else {
                                    for event in text_input_events("/allowed-tools") {
                                        keys.try_send(event).unwrap();
                                    }
                                }
                                stage = 1;
                            }
                            1 if text.contains(command) && text.contains("Recently denied") => {
                                keys.try_send(key(KeyCode::Char('r'))).unwrap(); stage = 2;
                            }
                            2 if text.contains("(retry)") => {
                                keys.try_send(key(KeyCode::Esc)).unwrap(); stage = 3;
                            }
                            3 => {
                                let snapshot = probe.snapshot.lock().unwrap();
                                if let Some(snapshot) = snapshot.as_ref() {
                                    if !snapshot.local_command_open
                                        && snapshot.query_id.as_deref() == Some(query_id.as_str())
                                        && snapshot.model_messages.iter().any(|message| matches!(message,
                                            Message::User(user) if user.content.iter().any(|content| matches!(content,
                                                UserContent::MetaText(text) if text.contains("Permission granted for:")
                                            ))
                                        )) {
                                        stage = 4; break;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            assert_eq!(
                stage, 4,
                "retry stalled: immediate={start_active}, stage={stage}, canvas=\n{text}"
            );
            let snapshot = probe.snapshot.lock().unwrap();
            let messages = &snapshot.as_ref().unwrap().model_messages;
            let retry_index = messages.iter().position(|message| matches!(message,
                Message::System(SystemMessage::PermissionRetry { commands, content, .. })
                if commands == &[command.to_string()] && content == &format!("Allowed {command}")
            )).expect("typed retry system message must be persisted before onDone results");
            let meta_index = messages.iter().position(|message| matches!(message,
                Message::User(user) if user.content.iter().any(|content| matches!(content,
                    UserContent::MetaText(text) if text == &format!("Permission granted for: {command}. You may now retry this command if you would like.")
                ))
            )).expect("retry instruction must retain user meta semantics");
            assert!(retry_index < meta_index);
            let started = probe.started_queries.lock().unwrap();
            if start_active {
                assert!(
                    started.is_empty(),
                    "immediate completion must not launch another query"
                );
                assert_eq!(
                    meta_index,
                    retry_index + 1,
                    "undefined immediate result creates no command display messages"
                );
                let state = store.get();
                assert!(
                    !state
                        .notifications
                        .current
                        .iter()
                        .chain(state.notifications.queue.iter())
                        .any(|notification| notification.key == "immediate-permissions")
                );
            } else {
                assert_eq!(started.len(), 1);
                let params = &started[0];
                assert_eq!(
                    params.input, "/allowed-tools",
                    "original alias must survive callback continuation"
                );
                assert_eq!(&params.model_messages, messages);
                assert_eq!(
                    meta_index,
                    retry_index + 3,
                    "normal onDone adds command input and no-content stdout before meta"
                );
                assert!(params.tool_use_context.add_notification.0.is_some());
                assert!(!params.tool_use_context.commands.is_empty());
                assert!(params.tool_use_context.app_store.store.is_some());
                let api = crate::utils::messages::normalize_messages_for_api(
                    params.model_messages.clone(),
                );
                assert!(
                    !api.iter()
                        .any(|message| matches!(message, Message::System(_)))
                );
            }
            assert!(!abort.is_aborted());
            assert!(matches!(
                query_command_rx.try_recv(),
                Err(async_channel::TryRecvError::Empty)
            ));
            drop(frames);
            drop(query_events);
        }
    }

    #[test]
    fn resume_empty_picker_completes_with_official_on_done_output_not_cancel() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let config_home =
            std::env::temp_dir().join(format!("cometix-repl-resume-empty-{}", Uuid::new_v4()));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        // The picker loads on worker threads now (CC awaits it), so wait for
        // the onDone output instead of an idle window the load can outlast.
        let canvases = run_repl_script(
            element!(ReplHarness).into_any(),
            vec![
                script_step(&[], text_input_events("/resume")),
                script_step(&["No conversations found to resume"], Vec::new()),
            ],
        );
        let text = canvas_lines(
            canvases
                .last()
                .expect("mock render should produce a final canvas"),
        )
        .join("\n");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            text.contains("/resume"),
            "empty resume picker should append the command row; canvas=\n{text}"
        );
        assert!(
            text.contains("No conversations found to resume"),
            "empty resume picker should append official onDone output; canvas=\n{text}"
        );
        assert!(
            !text.contains("Resume cancelled"),
            "official empty-log onDone path must not fall through to cancel output; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after empty resume picker completion; canvas=\n{text}"
        );
        assert!(
            !text.contains("Resume Session"),
            "empty resume picker should not leave the selector panel mounted; canvas=\n{text}"
        );
        assert!(
            !config_home.join("projects").exists(),
            "empty readonly resume picker should not create session project directories"
        );
    }

    #[test]
    fn resume_picker_enter_applies_selected_session_without_session_write() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let config_home =
            std::env::temp_dir().join(format!("cometix-repl-resume-select-{}", Uuid::new_v4()));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let project_path = resume::current_project_path();
        let session_id = format!("resume-select-{}", Uuid::new_v4());
        let prompt_text = "selected restore target prompt";
        let assistant_text = "selected restore target assistant reply";
        let session_file =
            write_resume_fixture(&project_path, &session_id, prompt_text, assistant_text);
        let before = std::fs::read_to_string(&session_file)
            .expect("session fixture should be readable before resume");

        let focused_fixture = format!("❯ {prompt_text}");
        let canvases = run_repl_script(
            element!(ReplHarness).into_any(),
            vec![
                script_step(&[], text_input_events("/resume")),
                // Enter only once the loaded picker has the fixture focused.
                script_step(&["Resume Session", &focused_fixture], vec![key(KeyCode::Enter)]),
                script_step(&[assistant_text], Vec::new()),
            ],
        );
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();
        let text = rendered
            .last()
            .expect("mock render should produce a final canvas")
            .clone();
        let after = std::fs::read_to_string(&session_file)
            .expect("session fixture should remain readable after resume");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            rendered
                .iter()
                .any(|frame| frame.contains("Resume Session") && frame.contains(prompt_text)),
            "resume picker should show the fixture before selection; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
        assert!(
            text.contains(prompt_text),
            "selected resume session should replace transcript with restored user prompt; canvas=\n{text}"
        );
        assert!(
            text.contains(assistant_text),
            "selected resume session should restore assistant transcript rows; canvas=\n{text}"
        );
        assert!(
            !text.contains("Resume Session"),
            "selector should be cleared after selecting a session; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after selecting a session; canvas=\n{text}"
        );
        assert_eq!(
            after, before,
            "readonly resume selection must not mutate the session JSONL fixture"
        );
    }

    #[test]
    fn resume_picker_selection_error_appends_official_on_done_output_without_system_notice() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let config_home = std::env::temp_dir().join(format!(
            "cometix-repl-resume-select-error-{}",
            Uuid::new_v4()
        ));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let project_path = resume::current_project_path();
        let session_id = Uuid::new_v4().to_string();
        let session_file = write_empty_resume_fixture(&project_path, &session_id);
        let before = std::fs::read_to_string(&session_file)
            .expect("empty session fixture should be readable before picker selection");

        let canvases = run_repl_script(
            element!(ReplHarness).into_any(),
            vec![
                script_step(&[], text_input_events("/resume")),
                // The picker only mounts once loading finished, with the lone
                // fixture focused.
                script_step(&["Resume Session"], vec![key(KeyCode::Enter)]),
                script_step(&["Failed to resume"], Vec::new()),
            ],
        );
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();
        let text = rendered
            .last()
            .expect("mock render should produce a final canvas")
            .clone();
        let after = std::fs::read_to_string(&session_file)
            .expect("empty session fixture should remain readable after picker selection");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            rendered
                .iter()
                .any(|frame| frame.contains("Resume Session")),
            "picker should show the empty fixture before selection; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
        assert!(
            text.contains("/resume"),
            "picker selection failure should append the command row; canvas=\n{text}"
        );
        // CC resume.tsx:233-235 includes the failure prefix. Compare the
        // complete receipt across terminal wrapping, including this session ID.
        let wrapped_text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            wrapped_text.contains(&format!(
                "Failed to resume: Session {session_id} has no transcript messages."
            )),
            "picker selection failure should append the complete official local command output; canvas=\n{text}"
        );
        assert!(
            !text.contains("Resume Session"),
            "selector should be cleared after picker selection failure; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after picker selection failure; canvas=\n{text}"
        );
        assert_eq!(
            after, before,
            "readonly picker selection failure must not mutate the session JSONL fixture"
        );
    }

    #[test]
    fn resume_by_id_restores_without_picker_or_session_write() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        // The fixture encodes the process cwd; the lookup reads the project
        // dir — pin them to the same root.
        let _project_dir = crate::utils::env_utils::PinnedProjectDir::at_manifest_root();
        let config_home =
            std::env::temp_dir().join(format!("cometix-repl-resume-id-{}", Uuid::new_v4()));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let project_path = resume::current_project_path();
        let session_id = Uuid::new_v4().to_string();
        let prompt_text = "direct resume id prompt";
        let assistant_text = "direct resume id assistant reply";
        let session_file =
            write_resume_fixture(&project_path, &session_id, prompt_text, assistant_text);
        let before = std::fs::read_to_string(&session_file)
            .expect("session fixture should be readable before direct resume");

        let command = format!("/resume {session_id}");
        let text = last_repl_text_waiting_for(
            stream::iter(text_input_events(&command)),
            assistant_text,
            10_000,
        );
        let after = std::fs::read_to_string(&session_file)
            .expect("session fixture should remain readable after direct resume");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            text.contains(prompt_text),
            "direct /resume <id> should restore the user prompt; canvas=\n{text}"
        );
        assert!(
            text.contains(assistant_text),
            "direct /resume <id> should restore assistant transcript rows; canvas=\n{text}"
        );
        assert!(
            !text.contains("Resume Session"),
            "direct /resume <id> should not render the selector; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after direct /resume <id>; canvas=\n{text}"
        );
        assert_eq!(
            after, before,
            "readonly direct /resume <id> must not mutate the session JSONL fixture"
        );
    }

    #[test]
    fn resume_by_exact_custom_title_restores_without_picker_or_session_write() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let _project_dir = crate::utils::env_utils::PinnedProjectDir::at_manifest_root();
        let config_home =
            std::env::temp_dir().join(format!("cometix-repl-resume-title-{}", Uuid::new_v4()));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let project_path = resume::current_project_path();
        let session_id = Uuid::new_v4().to_string();
        let custom_title = format!("Exact custom title {}", Uuid::new_v4());
        let prompt_text = "direct title resume prompt";
        let assistant_text = "direct title resume assistant reply";
        let session_file = write_resume_fixture_with_custom_title(
            &project_path,
            &session_id,
            prompt_text,
            assistant_text,
            Some(&custom_title),
        );
        let before = std::fs::read_to_string(&session_file)
            .expect("session fixture should be readable before direct title resume");

        let command = format!("/resume {custom_title}");
        // Settle on the restored transcript, not on render silence.
        let text = last_repl_text_waiting_for(
            stream::iter(text_input_events(&command)),
            assistant_text,
            10_000,
        );
        let after = std::fs::read_to_string(&session_file)
            .expect("session fixture should remain readable after direct title resume");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            text.contains(prompt_text),
            "direct /resume <title> should restore the user prompt; canvas=\n{text}"
        );
        assert!(
            text.contains(assistant_text),
            "direct /resume <title> should restore assistant transcript rows; canvas=\n{text}"
        );
        assert!(
            !text.contains("Resume Session"),
            "direct /resume <title> should not render the selector; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after direct /resume <title>; canvas=\n{text}"
        );
        assert_eq!(
            after, before,
            "readonly direct /resume <title> must not mutate the session JSONL fixture"
        );
    }

    #[test]
    fn resume_by_ambiguous_custom_title_appends_official_error_without_restoring() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let _project_dir = crate::utils::env_utils::PinnedProjectDir::at_manifest_root();
        let config_home = std::env::temp_dir().join(format!(
            "cometix-repl-resume-title-ambiguous-{}",
            Uuid::new_v4()
        ));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let project_path = resume::current_project_path();
        let custom_title = format!("Shared custom title {}", Uuid::new_v4());
        let first_session_id = Uuid::new_v4().to_string();
        let first_file = write_resume_fixture_with_custom_title(
            &project_path,
            &first_session_id,
            "ambiguous title first prompt",
            "ambiguous title first assistant reply",
            Some(&custom_title),
        );
        std::thread::sleep(Duration::from_millis(20));
        let second_session_id = Uuid::new_v4().to_string();
        let second_file = write_resume_fixture_with_custom_title(
            &project_path,
            &second_session_id,
            "ambiguous title second prompt",
            "ambiguous title second assistant reply",
            Some(&custom_title),
        );
        let first_before = std::fs::read_to_string(&first_file)
            .expect("first session fixture should be readable before ambiguous title resume");
        let second_before = std::fs::read_to_string(&second_file)
            .expect("second session fixture should be readable before ambiguous title resume");

        let command = format!("/resume {custom_title}");
        // Settle on the official error copy, not on render silence.
        let text = last_repl_text_waiting_for(
            stream::iter(text_input_events(&command)),
            "Found 2 sessions matching",
            10_000,
        );
        let first_after = std::fs::read_to_string(&first_file)
            .expect("first session fixture should remain readable after ambiguous title resume");
        let second_after = std::fs::read_to_string(&second_file)
            .expect("second session fixture should remain readable after ambiguous title resume");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            text.contains("/resume"),
            "ambiguous title error should append the command row; canvas=\n{text}"
        );
        assert!(
            text.contains("Found 2 sessions matching"),
            "ambiguous title error should use official multiple-match copy; canvas=\n{text}"
        );
        assert!(
            text.contains("Please use /resume to pick a specific") && text.contains("session."),
            "ambiguous title error should guide back to the picker across normal terminal wrapping; canvas=\n{text}"
        );
        assert!(
            !text.contains("ambiguous title first assistant reply")
                && !text.contains("ambiguous title second assistant reply"),
            "ambiguous title error must not restore either transcript; canvas=\n{text}"
        );
        assert!(
            !text.contains("Resume Session"),
            "ambiguous direct /resume <title> should not leave the selector mounted; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after ambiguous title error; canvas=\n{text}"
        );
        assert_eq!(
            first_after, first_before,
            "ambiguous title path must not mutate the first session fixture"
        );
        assert_eq!(
            second_after, second_before,
            "ambiguous title path must not mutate the second session fixture"
        );
    }

    #[test]
    fn resume_by_missing_arg_appends_official_error_without_selector_or_session_write() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let _project_dir = crate::utils::env_utils::PinnedProjectDir::at_manifest_root();
        let config_home =
            std::env::temp_dir().join(format!("cometix-repl-resume-missing-{}", Uuid::new_v4()));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let project_path = resume::current_project_path();
        let session_id = Uuid::new_v4().to_string();
        let session_file = write_resume_fixture(
            &project_path,
            &session_id,
            "existing resume fixture prompt",
            "existing resume fixture assistant reply",
        );
        let before = std::fs::read_to_string(&session_file)
            .expect("session fixture should be readable before missing-arg resume");
        let missing_arg = format!("missing-{}", Uuid::new_v4());
        let command = format!("/resume {missing_arg}");

        // Settle on the official error copy, not on render silence.
        let text = last_repl_text_waiting_for(
            stream::iter(text_input_events(&command)),
            "was not found.",
            10_000,
        );
        let after = std::fs::read_to_string(&session_file)
            .expect("session fixture should remain readable after missing-arg resume");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            text.contains("/resume"),
            "missing-arg error should append the command row; canvas=\n{text}"
        );
        assert!(
            text.contains(&format!("Session {missing_arg} was not found.")),
            "missing-arg error should use official session-not-found copy; canvas=\n{text}"
        );
        assert!(
            !text.contains("Resume Session"),
            "direct missing /resume <arg> should not render the selector; canvas=\n{text}"
        );
        assert!(
            !text.contains("existing resume fixture assistant reply"),
            "missing-arg error must not restore the existing fixture transcript; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after missing-arg error; canvas=\n{text}"
        );
        assert_eq!(
            after, before,
            "missing-arg path must not mutate the session JSONL fixture"
        );
    }

    #[test]
    fn resume_picker_filters_restored_current_session_on_next_open_without_writes() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let config_home =
            std::env::temp_dir().join(format!("cometix-repl-resume-filter-{}", Uuid::new_v4()));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let project_path = resume::current_project_path();
        let older_session_id = format!("resume-filter-older-{}", Uuid::new_v4());
        let older_prompt = "remaining resume option prompt";
        let older_file = write_resume_fixture(
            &project_path,
            &older_session_id,
            older_prompt,
            "remaining resume option assistant reply",
        );
        std::thread::sleep(Duration::from_millis(20));
        let current_session_id = format!("resume-filter-current-{}", Uuid::new_v4());
        let current_prompt = "restored current session prompt";
        let current_file = write_resume_fixture(
            &project_path,
            &current_session_id,
            current_prompt,
            "restored current session assistant reply",
        );
        let older_before = std::fs::read_to_string(&older_file)
            .expect("older session fixture should be readable before resume");
        let current_before = std::fs::read_to_string(&current_file)
            .expect("current session fixture should be readable before resume");

        // Newest first: the restored-to-be current session is the focused row.
        let focused_current = format!("❯ {current_prompt}");
        let canvases = run_repl_script(
            element!(ReplHarness).into_any(),
            vec![
                script_step(&[], text_input_events("/resume")),
                script_step(&["Resume Session", &focused_current], vec![key(KeyCode::Enter)]),
                // Reopen only after the selection restored the transcript.
                script_step(
                    &["restored current session assistant reply"],
                    text_input_events("/resume"),
                ),
                script_step(&["Resume Session", older_prompt], Vec::new()),
            ],
        );
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();
        let text = rendered
            .last()
            .expect("mock render should produce a final canvas")
            .clone();
        let older_after = std::fs::read_to_string(&older_file)
            .expect("older session fixture should remain readable after resume");
        let current_after = std::fs::read_to_string(&current_file)
            .expect("current session fixture should remain readable after resume");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            rendered.iter().any(|frame| {
                frame.contains(current_prompt)
                    && frame.contains("restored current session assistant reply")
                    && !frame.contains("Resume Session")
            }),
            "first selection should restore the newest current-project session before reopening; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
        assert!(
            text.contains("Resume Session"),
            "second /resume should reopen the selector; canvas=\n{text}"
        );
        assert!(
            text.contains(older_prompt),
            "selector should keep other sessions available after current-session filtering; canvas=\n{text}"
        );
        let picker_text = text
            .split_once("Resume Session")
            .map(|(_, picker)| picker)
            .expect("second selector should have a picker region");
        assert!(
            !picker_text.contains(current_prompt),
            "official current-session filter should exclude the restored session from the next picker (the restored transcript remains above it); canvas=\n{text}"
        );
        assert_eq!(
            older_after, older_before,
            "current-session filter path must not mutate the older session fixture"
        );
        assert_eq!(
            current_after, current_before,
            "current-session filter path must not mutate the restored session fixture"
        );
    }

    #[test]
    fn resume_cross_project_selection_appends_official_on_done_output_without_restoring() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _ssh = EnvVarGuard::set("SSH_CONNECTION", "fixture");
        let _tmux = EnvVarGuard::unset("TMUX");
        let config_home =
            std::env::temp_dir().join(format!("cometix-repl-resume-cross-{}", Uuid::new_v4()));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(
            config_home.join("projects"),
        );
        let _write_guard = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");

        let current_project = resume::current_project_path();
        let current_session_id = Uuid::new_v4().to_string();
        let current_file = write_resume_fixture(
            &current_project,
            &current_session_id,
            "current project resume prompt",
            "current project assistant reply",
        );
        std::thread::sleep(Duration::from_millis(20));
        let other_project = std::env::temp_dir()
            .join(format!("cometix-cross-project-{}", Uuid::new_v4()))
            .display()
            .to_string();
        let other_session_id = Uuid::new_v4().to_string();
        let other_prompt = format!("cross project target prompt {}", Uuid::new_v4());
        let other_assistant = "cross project assistant should not restore";
        let other_file = write_resume_fixture(
            &other_project,
            &other_session_id,
            &other_prompt,
            other_assistant,
        );
        let current_before = std::fs::read_to_string(&current_file)
            .expect("current project fixture should be readable before resume");
        let other_before = std::fs::read_to_string(&other_file)
            .expect("cross project fixture should be readable before resume");

        // Match the retained production root's imported clipboard executor;
        // SSH + no tmux exercises OSC without touching the system clipboard.
        let app = element! { ReplHarness };
        let app = element! {
            ContextProvider(value: Context::owned(iocraft::Clipboard::new(Arc::new(crate::utils::exec_file_no_throw::ExecFileClipboardBackend)))) { #(app) }
        };
        let search_query = format!("⌕ {other_prompt}");
        let focused_other = format!("❯ {other_prompt}");
        let canvases = run_repl_script(
            app.into_any(),
            vec![
                script_step(&[], text_input_events("/resume")),
                script_step(
                    &["Resume Session", "current project resume prompt"],
                    vec![ctrl_key('a')],
                ),
                // The all-projects reload has landed once the other project's
                // session is listed; only then type the search.
                script_step(
                    &["Resume Session", &other_prompt],
                    other_prompt.chars().map(|ch| key(KeyCode::Char(ch))).collect(),
                ),
                script_step(
                    &[&search_query],
                    vec![key(KeyCode::Down), key(KeyCode::Down)],
                ),
                script_step(&[&focused_other], vec![key(KeyCode::Enter)]),
                script_step(
                    &["This conversation is from a different directory."],
                    Vec::new(),
                ),
            ],
        );
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();
        let text = rendered
            .last()
            .expect("mock render should produce a final canvas")
            .clone();
        let current_after = std::fs::read_to_string(&current_file)
            .expect("current project fixture should remain readable after resume");
        let other_after = std::fs::read_to_string(&other_file)
            .expect("cross project fixture should remain readable after resume");
        let _ = std::fs::remove_dir_all(&config_home);

        assert!(
            rendered
                .iter()
                .any(|frame| { frame.contains("Resume Session") && frame.contains(&other_prompt) }),
            "show-all-projects search should reveal the cross-project session; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
        assert!(
            text.contains("/resume"),
            "cross-project completion should append the command row; canvas=\n{text}"
        );
        assert!(
            text.contains("This conversation is from a different directory."),
            "cross-project completion should append official guidance; canvas=\n{text}"
        );
        let compact_text = text.split_whitespace().collect::<String>();
        assert!(
            compact_text.contains(&format!("claude--resume{other_session_id}")),
            "cross-project guidance should include the resume command; canvas=\n{text}"
        );
        assert!(
            text.contains("Command copied to clipboard"),
            "cross-project guidance should confirm the copied command; canvas=\n{text}"
        );
        assert!(
            !text.contains(other_assistant),
            "different-project selection should not restore the transcript in place; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should be restored after cross-project onDone output; canvas=\n{text}"
        );
        assert_eq!(
            current_after, current_before,
            "readonly cross-project path must not mutate the current-project fixture"
        );
        assert_eq!(
            other_after, other_before,
            "readonly cross-project path must not mutate the cross-project fixture"
        );
    }

    /// A context-scoped test observer for one UUID-tagged late callback.
    /// Its guard reports the actual Handler return, including first-settle's
    /// early return, after the consumer has awaited the real writer ACK.
    #[derive(Clone)]
    pub(super) struct ClipboardCallbackObserver {
        session_id: String,
        returned: Arc<std::sync::Mutex<Option<futures::channel::oneshot::Sender<()>>>>,
    }

    impl ClipboardCallbackObserver {
        pub(super) fn observe(&self, output: &str) -> Option<ClipboardCallbackReturn> {
            if !output.contains(&self.session_id) {
                return None;
            }
            self.returned
                .lock()
                .unwrap()
                .take()
                .map(|returned| ClipboardCallbackReturn(Some(returned)))
        }
    }

    pub(super) struct ClipboardCallbackReturn(Option<futures::channel::oneshot::Sender<()>>);
    impl Drop for ClipboardCallbackReturn {
        fn drop(&mut self) {
            if let Some(returned) = self.0.take() {
                let _ = returned.send(());
            }
        }
    }

    /// Test-only imported process completion clock. All clipboard policy and
    /// raw output remain in the real iocraft service and retained REPL writer.
    struct ResumeClipboardCompletionBackend {
        input: Arc<std::sync::Mutex<Option<String>>>,
        finished: Arc<std::sync::atomic::AtomicBool>,
        completion: std::sync::Mutex<Option<futures::channel::oneshot::Receiver<()>>>,
    }

    impl iocraft::ClipboardBackend for ResumeClipboardCompletionBackend {
        fn execute(
            &self,
            program: &str,
            args: &[&str],
            input: &str,
            timeout: Duration,
        ) -> futures::future::BoxFuture<'static, i32> {
            assert_eq!(program, "tmux", "SSH fixture must skip native clipboard");
            assert!(args == ["load-buffer", "-w", "-"] || args == ["load-buffer", "-"]);
            assert_eq!(timeout, Duration::from_secs(2));
            *self.input.lock().unwrap() = Some(input.to_string());
            let completion = self
                .completion
                .lock()
                .unwrap()
                .take()
                .expect("one clipboard request");
            let finished = self.finished.clone();
            Box::pin(async move {
                // Only the external completion clock is controlled; this
                // future still runs on the actual process-lifetime runtime.
                completion
                    .await
                    .expect("test releases clipboard completion");
                finished.store(true, std::sync::atomic::Ordering::SeqCst);
                0
            })
        }

        fn spawn(&self, future: futures::future::BoxFuture<'static, ()>) {
            iocraft::ClipboardBackend::spawn(
                &crate::utils::exec_file_no_throw::ExecFileClipboardBackend,
                future,
            );
        }

        fn is_kitty(&self) -> bool {
            iocraft::ClipboardBackend::is_kitty(
                &crate::utils::exec_file_no_throw::ExecFileClipboardBackend,
            )
        }
    }

    /// CC resume.tsx:155-174 and 225-235 share the same onDone Promise.
    /// A pending cross-project copy must lose to actual same-project restore
    /// completion, and its late callback must not settle a new export panel.
    #[cfg(unix)]
    #[test]
    fn resume_pending_clipboard_matches_official_restore_first_settlement() {
        let _guard = env_lock().lock().unwrap();
        let _project = crate::utils::env_utils::PinnedProjectDir::at_manifest_root();
        crate::utils::process_runtime::initialize_test_process_runtime();
        for fail_restore in [false, true] {
            let directory =
                std::env::temp_dir().join(format!("cometix-resume-copy-race-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", directory.join("config"));
            let _projects = crate::utils::session_storage::set_test_projects_dir_override(
                directory.join("projects"),
            );
            let _write = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
            let _ssh = EnvVarGuard::set("SSH_CONNECTION", "fixture");
            let _tmux = EnvVarGuard::set("TMUX", "fixture");
            let clipboard_input = Arc::new(std::sync::Mutex::new(None));
            let clipboard_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (release_clipboard, clipboard_completion) = futures::channel::oneshot::channel();
            let backend = Arc::new(ResumeClipboardCompletionBackend {
                input: clipboard_input.clone(),
                finished: clipboard_finished.clone(),
                completion: std::sync::Mutex::new(Some(clipboard_completion)),
            });
            let project = resume::current_project_path();
            let session = Uuid::new_v4().to_string();
            if fail_restore {
                write_empty_resume_fixture(&project, &session);
            } else {
                write_resume_fixture(
                    &project,
                    &session,
                    "same-project restore target",
                    "restored-before-late-clipboard",
                );
            }
            std::thread::sleep(Duration::from_millis(20));
            let cross_session = Uuid::new_v4().to_string();
            let (callback_returned, callback_return) = futures::channel::oneshot::channel();
            let callback_observer = ClipboardCallbackObserver {
                session_id: cross_session.clone(),
                returned: Arc::new(std::sync::Mutex::new(Some(callback_returned))),
            };
            write_resume_fixture(
                &directory.join("other-project").display().to_string(),
                &cross_session,
                "newest cross-project target",
                "cross-project must not restore",
            );
            let frames = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let rendered_width = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            futures::executor::block_on(async {
                let (sender, receiver) = async_channel::unbounded();
                let app = element! { ReplHarness };
                let mut app = element! {
                    ContextProvider(value: Context::owned(callback_observer)) {
                        ContextProvider(value: Context::owned(iocraft::Clipboard::new(backend))) { #(app) }
                    }
                };
                let mut output = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(receiver).with_size(120, 40),
                ));
                let collect = async {
                    while let Some(frame) = output.next().await {
                        frames.lock().unwrap().push(frame.to_string());
                        rendered_width.store(frame.width(), std::sync::atomic::Ordering::SeqCst);
                    }
                };
                let drive = async {
                    for event in text_input_events("/resume") {
                        sender.send(event).await.unwrap();
                    }
                    // Both picker loads run on worker threads (CC awaits
                    // them), so gate each key on the view it targets instead
                    // of a fixed sleep the load can outlast.
                    for _ in 0..1500 {
                        if frames
                            .lock()
                            .unwrap()
                            .last()
                            .is_some_and(|text| text.contains("Resume Session"))
                        {
                            break;
                        }
                        futures_timer::Delay::new(Duration::from_millis(10)).await;
                    }
                    sender.send(ctrl_key('a')).await.unwrap();
                    // The all-projects reload has landed once the newest row —
                    // the other project's session — is listed and focused.
                    for _ in 0..1500 {
                        if frames.lock().unwrap().last().is_some_and(|text| {
                            text.lines()
                                .any(|line| line.trim() == "❯ newest cross-project target")
                        }) {
                            break;
                        }
                        futures_timer::Delay::new(Duration::from_millis(10)).await;
                    }
                    sender.send(key(KeyCode::Enter)).await.unwrap();
                    for _ in 0..100 {
                        if clipboard_input.lock().unwrap().is_some() {
                            break;
                        }
                        futures_timer::Delay::new(Duration::from_millis(10)).await;
                    }
                    assert!(
                        clipboard_input.lock().unwrap().is_some(),
                        "cross-project copy did not start: {:?}",
                        frames.lock().unwrap()
                    );
                    // Keep the already rendered all-projects list. A filter
                    // toggle starts a separate async reload, so sleeping then
                    // pressing Enter does not prove which row was selected.
                    // CC checkCrossProjectResume also permits the current
                    // project's row while showAllProjects remains true.
                    sender.send(key(KeyCode::Down)).await.unwrap();
                    let expected_selection = if fail_restore {
                        // sessionStorage::enrichLog labels an empty log.
                        "❯ (session)"
                    } else {
                        "❯ same-project restore target"
                    };
                    for _ in 0..100 {
                        if frames.lock().unwrap().last().is_some_and(|text| {
                            text.lines().any(|line| line.trim() == expected_selection)
                        }) {
                            break;
                        }
                        futures_timer::Delay::new(Duration::from_millis(10)).await;
                    }
                    assert!(
                        frames.lock().unwrap().last().is_some_and(|text| {
                            text.lines().any(|line| line.trim() == expected_selection)
                        }),
                        "same-project row was not focused before selection: {:?}",
                        frames.lock().unwrap()
                    );
                    assert!(
                        !clipboard_finished.load(std::sync::atomic::Ordering::SeqCst),
                        "clipboard must still be pending at same-project selection"
                    );
                    sender.send(key(KeyCode::Enter)).await.unwrap();
                    let expected = if fail_restore {
                        "Failed to resume:"
                    } else {
                        "restored-before-late-clipboard"
                    };
                    for _ in 0..100 {
                        if frames.lock().unwrap().last().is_some_and(|text| {
                            text.contains(expected) && !text.contains("Resume Session")
                        }) {
                            break;
                        }
                        futures_timer::Delay::new(Duration::from_millis(10)).await;
                    }
                    assert!(
                        frames
                            .lock()
                            .unwrap()
                            .last()
                            .is_some_and(|text| text.contains(expected)),
                        "restore did not finish: {:?}",
                        frames.lock().unwrap()
                    );
                    for event in text_input_events("/export") {
                        sender.send(event).await.unwrap();
                    }
                    for _ in 0..100 {
                        if frames
                            .lock()
                            .unwrap()
                            .last()
                            .is_some_and(|text| text.contains("Export Conversation"))
                        {
                            break;
                        }
                        futures_timer::Delay::new(Duration::from_millis(10)).await;
                    }
                    assert!(
                        frames
                            .lock()
                            .unwrap()
                            .last()
                            .is_some_and(|text| text.contains("Export Conversation")),
                        "new export did not mount: {:?}",
                        frames.lock().unwrap()
                    );
                    // CC resume.tsx:155-174 resolves the old copy after this
                    // newer local-command panel has mounted. Prove the ordering
                    // instead of allowing the fixture's timeout to settle early.
                    assert!(
                        !clipboard_finished.load(std::sync::atomic::Ordering::SeqCst),
                        "old clipboard finished before the newer export panel mounted"
                    );
                    release_clipboard
                        .send(())
                        .expect("clipboard is waiting for explicit completion");
                    crate::utils::race(
                        async {
                            callback_return
                                .await
                                .expect("matching late callback returned");
                        },
                        async {
                            futures_timer::Delay::new(Duration::from_secs(2)).await;
                            panic!("late callback did not return after writer ACK");
                        },
                    )
                    .await;
                    // Verify a frame rendered after the callback, so a broken
                    // late callback cannot mutate State behind a stale canvas.
                    sender.send(TerminalEvent::Resize(121, 40)).await.unwrap();
                    for _ in 0..200 {
                        if rendered_width.load(std::sync::atomic::Ordering::SeqCst) == 121 {
                            break;
                        }
                        futures_timer::Delay::new(Duration::from_millis(10)).await;
                    }
                    assert_eq!(
                        rendered_width.load(std::sync::atomic::Ordering::SeqCst),
                        121,
                        "post-callback frame must render before negative assertions"
                    );
                };
                crate::utils::race(collect, drive).await;
            });
            let frames = frames.lock().unwrap();
            let final_frame = frames.last().unwrap();
            assert!(clipboard_finished.load(std::sync::atomic::Ordering::SeqCst));
            assert!(
                !frames
                    .iter()
                    .any(|frame| frame.contains("cross-project must not restore")),
                "the current-project selection must never restore the cross-project transcript: {frames:?}"
            );
            assert!(
                final_frame.contains("Export Conversation"),
                "late copy closed newer panel: {frames:?}"
            );
            assert!(
                !frames
                    .iter()
                    .any(|frame| frame.contains("Command copied to clipboard")),
                "late copy appended a second receipt: {frames:?}"
            );
            std::fs::remove_dir_all(&directory).unwrap();
        }
    }

    #[test]
    fn plugin_refresh_commands_reach_live_repl_suggestions_without_remount() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _lock = env_lock().lock().unwrap();
        struct RemoteModeRestore(bool);
        impl Drop for RemoteModeRestore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_is_remote_mode(self.0);
            }
        }
        // This fixture tests the refresh writer -> subscribed REPL reader.
        // Skip unrelated initial discovery so it cannot overwrite the staged
        // refresh result; no query is submitted and no provider is contacted.
        let _remote = RemoteModeRestore(crate::bootstrap::state::get_is_remote_mode());
        crate::bootstrap::state::set_is_remote_mode(true);
        let store = crate::state::store::AppStore::new(Default::default(), None);
        let mut app = element! {
            ContextProvider(value: Context::owned(
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
            )) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    IsolatedAuthRepl(app_store: Some(store.clone()))
                }
            }
        };
        let (keys, events) = async_channel::unbounded();
        let mut stage = 0;
        let mut text = String::new();
        futures::executor::block_on(async {
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(140, 40),
            ));
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let Some(canvas) = crate::utils::race(frames.next(), async move {
                    futures_timer::Delay::new(remaining).await;
                    None
                })
                .await
                else {
                    break;
                };
                text = canvas_lines(&canvas).join("\n");
                if stage == 0 && text.contains('❯') {
                    store.replace_with(|state| {
                        let mut command = crate::commands::declared_commands_for_tests()
                            .into_iter()
                            .find(|command| command.name == "pr-comments")
                            .unwrap();
                        command.name = "zzrefresh:live".into();
                        command.description = "First refreshed plugin command".into();
                        command.source = crate::commands::CommandSource::Plugin;
                        Arc::make_mut(&mut state.plugins).commands = Arc::new(vec![command]);
                    });
                    keys.try_send(TerminalEvent::Paste("/zzrefresh".into()))
                        .unwrap();
                    stage = 1;
                } else if stage == 1 && text.contains("First refreshed plugin command") {
                    store.replace_with(|state| {
                        let mut command = crate::commands::declared_commands_for_tests()
                            .into_iter()
                            .find(|command| command.name == "pr-comments")
                            .unwrap();
                        command.name = "zzrefresh:live".into();
                        command.description = "Second refreshed plugin command".into();
                        command.source = crate::commands::CommandSource::Plugin;
                        Arc::make_mut(&mut state.plugins).commands = Arc::new(vec![command]);
                    });
                    stage = 2;
                } else if stage == 2 && text.contains("Second refreshed plugin command") {
                    stage = 3;
                    break;
                }
            }
        });
        assert_eq!(
            stage, 3,
            "live plugin array must drive current suggestions: {text}"
        );
        assert!(
            !text.contains("First refreshed plugin command"),
            "stale metadata remained: {text}"
        );
    }

    #[test]
    fn terminal_setup_uses_official_on_done_null_transcript_path_without_panel() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _terminal = crate::utils::env_utils::EnvVarGuard::set("TERM", "xterm-kitty");
        let _cursor = crate::utils::env_utils::EnvVarGuard::unset("CURSOR_TRACE_ID");
        let _askpass = crate::utils::env_utils::EnvVarGuard::unset("VSCODE_GIT_ASKPASS_MAIN");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _runtime = runtime.enter();
        let text = last_repl_text_waiting_for(
            stream::iter(
                std::iter::once(TerminalEvent::Resize(120, 40))
                    .chain(text_input_events("/terminal-setup")),
            ),
            "Shift+Enter",
            10_000,
        );

        assert!(
            text.contains("/terminal-setup"),
            "terminal setup should append the command row; canvas=\n{text}"
        );
        assert!(
            text.contains("Shift+Enter"),
            "terminal setup should append official onDone output; canvas=\n{text}"
        );
        assert!(
            text.contains("❯"),
            "PromptInput should remain/restored after onDone-null local command output; canvas=\n{text}"
        );
        assert!(
            !text.contains("Esc to close"),
            "terminal setup should not render the old persistent panel; canvas=\n{text}"
        );
    }

    #[test]
    fn terminal_setup_matches_official_live_installer_receipt_and_file() {
        use crate::utils::env_utils::EnvVarGuard;
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("terminal-command-{}", uuid::Uuid::new_v4()));
        let fixture = install_keybindings_test_env(root.clone());
        let _vars = [
            EnvVarGuard::set("TERM", "xterm-256color"),
            EnvVarGuard::set("TERM_PROGRAM", "alacritty"),
            EnvVarGuard::unset("CURSOR_TRACE_ID"),
            EnvVarGuard::unset("VSCODE_GIT_ASKPASS_MAIN"),
            EnvVarGuard::set("XDG_CONFIG_HOME", root.join("xdg")),
        ];
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _runtime = runtime.enter();
        let text = last_repl_text_waiting_for(
            stream::iter(
                std::iter::once(TerminalEvent::Resize(120, 40))
                    .chain(text_input_events("/terminal-setup")),
            ),
            "Installed Alacritty Shift+Enter key binding",
            10_000,
        );
        assert!(
            text.contains("Installed Alacritty Shift+Enter key binding"),
            "{text}"
        );
        assert!(
            text.contains("❯"),
            "input must return after installer: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("xdg/alacritty/alacritty.toml")).unwrap(),
            "\n[[keyboard.bindings]]\nkey = \"Return\"\nmods = \"Shift\"\nchars = \"\\u001B\\r\"\n"
        );
        assert_eq!(
            crate::utils::config::load_global_config().shift_enter_key_binding_installed,
            Some(true)
        );
        drop(fixture);
    }

    #[test]
    fn prompt_help_question_mark_stays_in_prompt_footer_without_transcript_rows() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let timed_events = vec![(key(KeyCode::Char('?')), 0), (key(KeyCode::Esc), 50)];
        let event_stream = stream::unfold(timed_events.into_iter(), |mut events| async move {
            let (event, delay_ms) = events.next()?;
            if delay_ms > 0 {
                futures_timer::Delay::new(Duration::from_millis(delay_ms)).await;
            }
            Some((event, events))
        });
        let canvases = collect_repl_canvases(event_stream, 100, 12);
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();

        assert!(
            rendered.iter().any(|text| {
                text.contains("! for bash mode")
                    && text.contains("ctrl + o for verbose output")
                    && text.contains("❯")
            }),
            "prompt help should render under PromptInput in the footer area; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );

        let last = rendered
            .last()
            .expect("mock render should produce a final canvas");
        assert!(
            last.contains("? for shortcuts"),
            "Esc should restore the normal PromptInput footer; canvas=\n{last}"
        );
        assert!(
            !last.contains("Help dialog dismissed") && !last.contains("/help"),
            "prompt '?' help is not a local command invocation and should not append transcript rows; canvas=\n{last}"
        );
        assert!(
            !last.contains("! for bash mode"),
            "Esc should close the PromptInput footer help; canvas=\n{last}"
        );
    }

    #[test]
    fn logo_header_hot_path_avoids_uncached_auth_io_during_idle_input_and_suggestions() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        crate::components::messages_list::reset_logo_header_render_count();
        crate::components::logo_v2::logo_v2::reset_logo_display_data_current_call_count();
        crate::utils::auth::reset_auth_io_probe();

        let (event_tx, event_rx) = futures::channel::mpsc::unbounded::<TerminalEvent>();
        let rendered = futures::executor::block_on(async {
            let mut app = element!(ReplLogoHotPathHarness);
            let mut render_loop =
                Box::pin(app.mock_terminal_render_loop(MockTerminalConfig::with_events(event_rx)));

            let first = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_secs(2)).await;
                None
            })
            .await
            .expect("initial REPL frame");
            assert!(first.to_string().contains("Cometix Code"));

            // The first frame may read startup/cached display inputs. From this
            // point onward idle parent updates, prompt input, and suggestion
            // navigation must not re-enter Logo or uncached auth boundaries.
            crate::components::messages_list::reset_logo_header_render_count();
            crate::components::logo_v2::logo_v2::reset_logo_display_data_current_call_count();
            crate::utils::suggestions::command_suggestions::reset_command_suggestion_probe();
            crate::utils::auth::reset_auth_io_probe();

            let sender = std::thread::spawn(move || {
                for event in [
                    key(KeyCode::Char('/')),
                    key(KeyCode::Down),
                    key(KeyCode::Up),
                    key(KeyCode::Esc),
                ] {
                    std::thread::sleep(Duration::from_millis(40));
                    let _ = event_tx.unbounded_send(event);
                }
            });

            let mut rendered = Vec::new();
            while rendered.len() < 30 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(300)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                rendered.push(canvas_lines(&canvas).join("\n"));
            }
            sender.join().expect("terminal event sender");
            rendered
        });

        assert!(
            rendered
                .iter()
                .any(|text| text.contains("/add-dir") && text.contains("/advisor")),
            "slash suggestions should render during the probe; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
        assert!(
            rendered
                .last()
                .is_some_and(|text| !text.contains("/add-dir") && text.contains("❯ /")),
            "Esc must dismiss the memoized suggestions without changing input; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
        assert_eq!(
            crate::components::messages_list::logo_header_render_count(),
            0,
            "React.memo(LogoHeader) equivalent must retain idle/input frames"
        );
        assert_eq!(
            crate::components::logo_v2::logo_v2::logo_display_data_current_call_count(),
            0,
            "LogoV2 display helpers must not run on input/suggestion frames"
        );
        assert_eq!(
            crate::utils::suggestions::command_suggestions::command_suggestion_probe(),
            (0, 1),
            "stable commands must reuse one cached index and one '/' search across navigation"
        );
        assert_eq!(
            crate::utils::auth::auth_io_probe_snapshot(),
            crate::utils::auth::AuthIoProbeSnapshot::default(),
            "hot REPL frames must not read token files/settings or launch auth subprocesses"
        );
    }

    #[test]
    fn prompt_footer_omits_interrupt_hint_while_query_is_loading() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        // Submitting now awaits the UserPromptSubmit hooks, and this repository
        // configures one; without the pin the render would try to spawn it.
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _settings = crate::utils::env_utils::IsolatedProjectSettings::pin();
        let event_stream = stream::iter(text_input_events("hello during query"));
        let canvases = collect_repl_spinner_owner_canvases(event_stream, 80, 12);
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();

        let loading_footer = rendered
            .iter()
            .find(|text| text.contains("Repl-owned-spinner…"))
            .unwrap_or_else(|| {
                panic!(
                    "REPL should render its SpinnerWithVerb sibling while loading; canvases=\n{}",
                    rendered.join("\n--- frame ---\n")
                )
            });

        // Cometix-specific deviation (product requirement — skip in parity
        // audits): CC shows "esc to interrupt" while loading; not cloned.
        assert!(
            !loading_footer.contains("esc to interrupt"),
            "the interrupt tip is deliberately not cloned; canvas=\n{loading_footer}"
        );
        assert!(
            !loading_footer.contains("? for shortcuts"),
            "the idle shortcut hint must stay hidden while loading; canvas=\n{loading_footer}"
        );
    }

    #[test]
    fn immediate_local_command_ui_during_query_keeps_prompt_input_visible() {
        let canvases = futures::executor::block_on(
            element!(ImmediateLocalCommandUiDuringQueryProbe)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(100, 30))
                .collect::<Vec<_>>(),
        );
        let rendered = canvases
            .iter()
            .map(canvas_lines)
            .map(|lines| lines.join("\n"))
            .collect::<Vec<_>>();
        assert!(
            rendered.iter().any(|text| {
                // Cometix-specific deviation (product requirement — skip in
                // parity audits): the "esc to interrupt" loading tip is not
                // cloned, so the mounted PromptInput is anchored by its
                // prompt glyph instead.
                text.contains("slow query active smoke")
                    && text.contains("Settings")
                    && text.contains("Status")
                    && text.contains("❯")
            }),
            "immediate local command UI should keep PromptInput mounted while preserving context; canvases=\n{}",
            rendered.join("\n--- frame ---\n")
        );
    }

    #[test]
    fn local_command_ui_root_preserves_context_and_natural_height() {
        let canvases = futures::executor::block_on(
            element!(LocalCommandUiRootProbe)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(125, 41))
                .collect::<Vec<_>>(),
        );
        let last = canvases
            .last()
            .expect("mock render should produce a canvas");
        let lines = canvas_lines(last);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("transcript context stays visible")),
            "local command UI panels should preserve transcript context above the panel; canvas=\n{}",
            lines.join("\n")
        );
        let first_non_blank = first_non_blank_row(last).expect("panel should render visible cells");
        assert!(
            first_non_blank <= 1,
            "local command UI panels should stay in normal document flow, not be pushed to the bottom; first non-blank row={first_non_blank}, canvas height={}",
            last.height()
        );
        let last_non_blank = last_non_blank_row(last).expect("panel should render visible cells");
        assert!(
            last.height().saturating_sub(last_non_blank + 1) <= 1,
            "local command UI panels should not leave trailing blank rows; canvas height={}, last non-blank row={}",
            last.height(),
            last_non_blank
        );
    }
    /// CC clear/conversation.ts:170-200 preserves prev.toolPermissionContext.
    /// Drive the real slash-command handler so a pre-clear reset cannot hide
    /// behind a green unit test of clear_conversation alone.
    #[test]
    fn clear_retains_directory_grants_matches_official_repl_lifecycle() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _lock = env_lock().lock().unwrap();
        let _settings = crate::utils::env_utils::IsolatedProjectSettings::pin();
        let before_session = crate::bootstrap::state::get_session_id();
        let permission = crate::utils::permissions::permission_update::apply_permission_update(
            &ToolPermissionContext::default(),
            &crate::types::permissions::PermissionUpdate::AddDirectories {
                destination: crate::types::permissions::PermissionUpdateDestination::Session,
                directories: vec!["/z-granted-directory".into(), "/a-granted-directory".into()],
            },
        );
        let mut state = crate::state::app_state_store::AppState::default();
        state.set_tool_permission_context(permission.clone());
        let store = crate::state::store::AppStore::new(state, None);
        let mut events = "/clear"
            .chars()
            .map(|c| (key(KeyCode::Char(c)), 10))
            .collect::<Vec<_>>();
        events.push((key(KeyCode::Enter), 250));
        let theme = *theme::current();
        // Use the CLI's Tokio executor shape; clearConversation contains a
        // synchronous futures executor boundary and cannot nest in LocalPool.
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let mut app = element! {
                ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                    ContextProvider(value: Context::owned(theme)) {
                        IsolatedAuthRepl(app_store: Some(store.clone()))
                    }
                }
            };
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(timed_stream(events)).with_size(100, 32),
            ));
            for _ in 0..80 {
                if crate::bootstrap::state::get_session_id() != before_session {
                    break;
                }
                if crate::utils::race(frames.next(), async {
                    futures_timer::Delay::new(Duration::from_secs(3)).await;
                    None
                })
                .await
                .is_none()
                {
                    break;
                }
            }
        });
        assert_ne!(
            crate::bootstrap::state::get_session_id(),
            before_session,
            "the /clear handler must actually run"
        );
        assert_eq!(
            store
                .get()
                .tool_permission_context
                .additional_working_directories,
            permission.additional_working_directories,
        );
    }
    #[test]
    fn hot_resume_matches_official_hook_identity_cache_and_no_early_write() {
        // REPL.tsx:2398-2452,2478-2510: hooks precede switch; restoreReadFileState
        // consumes messages+hooks with log.projectPath, and no separate hook write.
        let _env = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let temp = BranchFixtureDirectory::new();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path().join("config"));
        let _project = crate::utils::env_utils::PinnedProjectDir::at(temp.path());
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let _remote = EnvVarGuard::set("CLAUDE_CODE_ENVIRONMENT_KIND", "byoc");
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        struct Restore(String, Option<PathBuf>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::switch_session(self.0.clone(), self.1.clone());
                crate::utils::session_storage::clear_session_metadata();
                crate::utils::session_storage::reset_session_file_pointer();
                crate::utils::hooks::hooks_config_snapshot::reset_hooks_config_snapshot();
                crate::utils::plans::clear_plans_directory_cache();
            }
        }
        let _restore = Restore(
            crate::bootstrap::state::get_session_id(),
            crate::bootstrap::state::get_session_project_dir(),
        );
        let old_id = Uuid::new_v4().to_string();
        let target_id = Uuid::new_v4().to_string();
        let old_dir = temp.path().join("old-store");
        let target_dir = temp.path().join("target-store");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        crate::bootstrap::state::switch_session(old_id.clone(), Some(old_dir.clone()));
        crate::utils::session_storage::clear_session_metadata();
        crate::utils::session_storage::reset_session_file_pointer();
        let expected_start_path =
            crate::utils::session_storage::get_transcript_path_for_session(&target_id);
        let marker = temp.path().join("hot-hook-inputs.jsonl");
        let quoted = marker.display().to_string().replace('\'', "'\\''");
        let command = format!(
            "cat >> '{quoted}'; printf '\\n' >> '{quoted}'; printf '%s' '{{\"systemMessage\":\"hot-resume-context\"}}'"
        );
        crate::utils::hooks::hooks_config_snapshot::capture_hooks_config_snapshot(
            &crate::utils::settings::SettingsJson {
                hooks: Some(json!({
                    "SessionEnd":[{"matcher":"*","hooks":[{"type":"command","command":command,"timeout":5}]}],
                    "SessionStart":[{"matcher":"resume","hooks":[{"type":"command","command":command,"timeout":5}]}]
                })),
                ..Default::default()
            },
            None,
            false,
        );
        let entries = vec![
            json!({"type":"user","uuid":"root","sessionId":target_id,"parentUuid":null,
                "timestamp":"2026-09-12T01:00:00Z","message":{"role":"user","content":"resume"}}),
            json!({"type":"assistant","uuid":"tools","sessionId":target_id,"parentUuid":"root",
            "timestamp":"2026-09-12T01:00:01Z","message":{"role":"assistant","content":[
                {"type":"tool_use","id":"read","name":"Read","input":{"file_path":"relative-read.txt"}},
                {"type":"tool_use","id":"write","name":"Write","input":{"file_path":"relative-write.txt","content":"written"}}
            ]}}),
            json!({"type":"user","uuid":"results","sessionId":target_id,"parentUuid":"tools",
            "timestamp":"2026-09-12T01:00:02Z","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"read","content":"     1→read content"},
                {"type":"tool_result","tool_use_id":"write","content":"written"}
            ]}}),
        ];
        let full_path = target_dir.join(format!("{target_id}.jsonl"));
        std::fs::write(
            &full_path,
            entries.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        let wrong_cache = crate::utils::query_helpers::extract_read_files_from_messages(
            &entries,
            "/selection-cwd",
            crate::utils::file_state_cache::READ_FILE_STATE_CACHE_SIZE,
        );
        assert_eq!(wrong_cache.len(), 2);
        // REPL.tsx:2359/2420 retains raw log.messages for plan recovery even
        // when deserializeMessages removes an interrupted ExitPlanMode call.
        crate::utils::plans::clear_plans_directory_cache();
        let mut original_messages = entries.clone();
        original_messages[0]["slug"] = json!("hot-interrupted-plan");
        original_messages.push(json!({"type":"assistant","uuid":"pending-plan",
            "message":{"role":"assistant","content":[{"type":"tool_use","id":"pending",
                "name":"ExitPlanMode","input":{"plan":"hot plan survives cleanup"}}]}}));
        let recovered_plan_path =
            crate::utils::plans::get_plans_directory().join("hot-interrupted-plan.md");
        let target = resume::ResumeTarget {
            session_id: target_id.clone(),
            project_path: Some("/log-cwd".into()),
            entries,
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: resume::ResumeMetadata {
                original_messages: Some(original_messages),
                session_id: Some(target_id.clone()),
                full_path: Some(full_path.display().to_string()),
                read_file_state: wrong_cache,
                ..Default::default()
            },
            entrypoint: Some(ResumeEntrypoint::SlashCommandPicker),
        };
        let processed = resume(target, None, Arc::new(Default::default()), None, None).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !recovered_plan_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(recovered_plan_path).unwrap(),
            "hot plan survives cleanup"
        );
        crate::utils::plans::clear_plan_slug(Some(&target_id));
        let inputs = serde_json::Deserializer::from_str(&std::fs::read_to_string(&marker).unwrap())
            .into_iter::<serde_json::Value>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0]["session_id"], old_id);
        assert_eq!(
            inputs[0]["transcript_path"],
            old_dir
                .join(format!("{old_id}.jsonl"))
                .display()
                .to_string()
        );
        assert_eq!(inputs[1]["session_id"], target_id);
        assert_eq!(
            inputs[1]["transcript_path"],
            expected_start_path.display().to_string()
        );
        assert_eq!(crate::bootstrap::state::get_session_id(), target_id);
        assert_eq!(
            crate::bootstrap::state::get_session_project_dir(),
            Some(target_dir)
        );
        let cache = &processed.resume_restore_stores.read_file_state;
        assert_eq!(cache.len(), 2);
        assert!(
            cache
                .iter()
                .all(|entry| entry.path.starts_with("/log-cwd/"))
        );
        assert!(
            processed
                .messages
                .iter()
                .any(|message| matches!(message, Message::HookResult(_)))
        );
        assert!(
            !std::fs::read_to_string(full_path)
                .unwrap()
                .contains("hot-resume-context"),
            "REPL appends hook messages to history; it must not pre-write them during resume"
        );
    }
}

#[cfg(test)]
mod raw_image_rewind_tests {
    use super::*;

    #[test]
    fn rewind_raw_images_match_official_source_filter_and_original_image_indices() {
        use crate::types::message::UserContent;
        let url = serde_json::json!({"type":"image","source":{"type":"url","url":"https://example.test/image.png"}});
        let base64 = serde_json::json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"},"cache_control":{"type":"ephemeral"}});
        let mut message = crate::utils::messages::create_user_message("before".into());
        message.content.extend([
            UserContent::from_image_block(url.clone(), false),
            UserContent::from_image_block(base64, false),
        ]);
        message.image_paste_ids = Some(vec![10, 20]);
        let update = restore_message_sync(&message, 1, "draft");
        let pasted = update.pasted_contents.unwrap();
        assert_eq!(pasted.len(), 1);
        assert!(pasted.contains_key(&20));
        message.content = vec![UserContent::from_image_block(url, false)];
        assert_eq!(
            restore_message_sync(&message, 2, "draft").pasted_contents,
            Some(BTreeMap::new())
        );
    }
}
