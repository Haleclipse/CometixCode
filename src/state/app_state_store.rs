//! Maps to: CC `state/AppStateStore.ts` — the `AppState` shape and
//! `getDefaultAppState()`.
//!
//! Incremental adoption: CC's AppState has 40+ fields; this starts with the
//! four that have `onChangeAppState` side effects (the choke-point fields)
//! and grows as REPL local state and the Runtime* contexts are absorbed.
//! Field-by-field mapping notes live on each field.
//!
//! Representation rules (the Rust equivalent of CC's reference semantics):
//! - Scalars are stored inline.
//! - Heavy fields are `Arc`-wrapped so `AppState::clone()` is a shallow
//!   refcount bump — the equivalent of CC's `{...prev}` spread — and the
//!   `on_change` diff can use `Arc::ptr_eq` where CC uses `!==`.
//! - CC's `DeepImmutable<>` is the type system here: snapshots hand out
//!   `Arc<AppState>`, mutation only happens inside `AppStore::set_state` /
//!   `replace_with` updaters (fresh clone per install).

use std::sync::Arc;

use crate::services::mcp::types::{McpServerConnection, McpServerConnectionType, ServerResource};
use crate::tool::ToolPermissionContext;

/// Maps to: CC `AppState.expandedView: 'none' | 'tasks' | 'teammates'`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExpandedView {
    #[default]
    None,
    Tasks,
    Teammates,
}

/// Maps to: CC `AppStateStore.ts` `FooterItem` — which footer pill is focused
/// via arrow-key navigation below the prompt. Lives in AppState so components
/// outside PromptInput (CC: CompanionSprite, CoordinatorAgentStatus) can read
/// their own focused state without prop-drilling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterItem {
    Tasks,
    Tmux,
    Bagel,
    Teams,
    Bridge,
    Companion,
}

impl FooterItem {
    /// Stable id used by footer left-side highlight / hint copy.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tasks => "tasks",
            Self::Tmux => "tmux",
            Self::Bagel => "bagel",
            Self::Teams => "teams",
            Self::Bridge => "bridge",
            Self::Companion => "companion",
        }
    }
}

/// Maps to: CC `AppState.viewSelectionMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewSelectionMode {
    #[default]
    None,
    SelectingAgent,
    ViewingAgent,
}

impl ViewSelectionMode {
    pub fn is_selecting(self) -> bool {
        matches!(self, Self::SelectingAgent)
    }
}

/// Maps to: CC `AppState.elicitation` — declared inline in
/// `state/AppStateStore.ts` as `{ queue: ElicitationRequestEvent[] }`.
///
/// The FIELD SHAPE belongs here, with the rest of AppState. Only the element
/// type is imported from elsewhere (`AppStateStore.ts:6`,
/// `import type { ElicitationRequestEvent } from '../services/mcp/elicitationHandler.js'`),
/// and that type keeps its own owner in `services/mcp/elicitation_handler.rs`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ElicitationState {
    pub queue: Vec<crate::services::mcp::elicitation_handler::ElicitationRequestEvent>,
}

impl ElicitationState {
    /// Maps to: CC appending an `ElicitationRequestEvent` in
    /// `registerElicitationHandler(...)`.
    pub fn push_event(
        &mut self,
        event: crate::services::mcp::elicitation_handler::ElicitationRequestEvent,
    ) {
        self.queue.push(event);
    }

    /// Maps to: CC `ElicitationCompleteNotificationSchema` queue update.
    pub fn mark_complete(&mut self, server_name: &str, elicitation_id: &str) -> bool {
        let (queue, found) = crate::services::mcp::elicitation_handler::mark_elicitation_complete(
            &self.queue,
            server_name,
            elicitation_id,
        );
        self.queue = queue;
        found
    }

    /// The request `request_id` of `server_name` leaves the queue: CC's
    /// dialog cancels a request whose `signal` aborts
    /// (`ElicitationDialog.tsx:193-209`), which then leaves the queue.
    pub fn remove(&mut self, server_name: &str, request_id: &str) -> bool {
        let before = self.queue.len();
        self.queue
            .retain(|event| !(event.server_name == server_name && event.request_id == request_id));
        self.queue.len() != before
    }

    /// Maps to: CC `prev.elicitation.queue.slice(1)` after response/dismiss.
    pub fn pop_front(
        &mut self,
    ) -> Option<crate::services::mcp::elicitation_handler::ElicitationRequestEvent> {
        (!self.queue.is_empty()).then(|| self.queue.remove(0))
    }
}

/// Maps to: CC `AppState.promptSuggestion`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptSuggestionState {
    pub text: Option<String>,
    /// `"user_intent"` | `"stated_intent"` | none.
    pub prompt_id: Option<String>,
    pub shown_at: u64,
    pub accepted_at: u64,
    pub generation_request_id: Option<String>,
}

// `TodoItem` / `TodoStatus` / `TodoList` are owned by `utils/todo/types.rs`,
// mirroring CC where `AppStateStore.ts:2` imports `TodoList` from
// `utils/todo/types.ts` rather than declaring it. Imported, not re-exported —
// CC re-exports nothing here, so consumers name the owner.
use crate::utils::todo::types::TodoItem;

/// Read-only stub for non-teammate task types until registries dissolve into a
/// unified `TaskState` (CC `tasks/types.ts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskStateOther {
    pub id: String,
    /// `"local_bash"` | `"local_agent"` | …
    pub task_type: String,
    pub status: String,
    pub description: String,
    /// Maps to: CC `TaskStateBase.isBackgrounded`.
    /// - `None` — field absent → never kill on `/clear` (CC `'isBackgrounded' in task`)
    /// - `Some(false)` — foreground → kill+remove on `/clear`
    /// - `Some(true)` — background → preserve
    pub is_backgrounded: Option<bool>,
    /// Maps to: CC `TaskStateBase.notified` — set once the model has consumed
    /// the terminal result; eviction guard (`utils/task/framework.ts:133`).
    /// P4 (2026-08-02): added for `evict_terminal_task` guard parity.
    pub notified: bool,
    /// Maps to: CC `LocalAgentTaskState.retain` — `None` = field absent, so
    /// CC's `'retain' in task` narrowing (utils/task/framework.ts:138) is
    /// false and the panel grace window does not apply.
    /// P4 (2026-08-02): added for `evict_terminal_task` guard parity.
    /// Producer: the local_agent registry mirror
    /// (`tasks/local_agent_task.rs::mirror_task_to_app_state`) always writes
    /// `Some(_)` — CC `LocalAgentTask.tsx:191` declares `retain: boolean` as
    /// a required field (created `retain: false`), so a mirrored local_agent
    /// task always has the field present.
    pub retain: Option<bool>,
    /// Maps to: CC `LocalAgentTaskState.evictAfter` (epoch ms) — panel grace
    /// deadline; `None` = unset (CC `task.evictAfter ?? Infinity`,
    /// utils/task/framework.ts:138).
    /// P4 (2026-08-02): added for `evict_terminal_task` guard parity.
    /// Producer: the local_agent registry mirror projects the registry's
    /// terminal-transition deadline (CC `LocalAgentTask.tsx:379/:526/:556`).
    pub evict_after: Option<u64>,
    /// Maps to: CC `LocalAgentTaskState.progress` (`LocalAgentTask.tsx:178`)
    /// — two-field projection (`progress.toolUseCount`); SEAM: the full
    /// progress object (activities, summary) lands with a future
    /// `TaskState::LocalAgent` variant.
    pub progress_tool_uses: Option<usize>,
    /// Maps to: CC `LocalAgentTaskState.progress` (`LocalAgentTask.tsx:178`)
    /// — two-field projection (`progress.tokenCount`); same SEAM as
    /// [`Self::progress_tool_uses`].
    pub progress_tokens: Option<u64>,
}

/// Maps to: CC `AppState.tasks[taskId]` — unified task map.
/// In-process teammate entries carry the spinner/UI snapshot subset until the
/// full `InProcessTeammateTaskState` (with abort controllers / messages) lives
/// here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskState {
    InProcessTeammate(crate::components::spinner::teammate_tree::TeammateTaskSnapshot),
    /// Maps to CC `tasks/LocalShellTask/guards.ts::LocalShellTaskState`.
    LocalShell(crate::tasks::local_shell_task::guards::LocalShellTaskState),
    /// Maps to CC `tasks/DreamTask/DreamTask.ts::DreamTaskState`.
    Dream(crate::tasks::dream_task::DreamTaskState),
    Other(TaskStateOther),
}

impl TaskState {
    pub fn id(&self) -> &str {
        match self {
            Self::InProcessTeammate(task) => task.id.as_str(),
            Self::LocalShell(task) => task.id.as_str(),
            Self::Dream(task) => task.id.as_str(),
            Self::Other(task) => task.id.as_str(),
        }
    }

    pub fn as_in_process_teammate(
        &self,
    ) -> Option<&crate::components::spinner::teammate_tree::TeammateTaskSnapshot> {
        match self {
            Self::InProcessTeammate(task) => Some(task),
            Self::LocalShell(_) | Self::Dream(_) | Self::Other(_) => None,
        }
    }
}

/// Maps to CC `AppState.mcp` (`AppStateStore.ts:173-183`), which declares the
/// bag inline in the `AppState` type rather than naming it — hence a struct
/// here and a field there. Tools/commands and per-server resources are
/// materialized when a client connects/updates; render/query consumers read
/// them directly rather than rebuilding projections from connection records.
///
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpState {
    /// Maps to: CC `AppStateStore.ts:183` / `getDefaultAppState:517`.
    pub plugin_reconnect_key: u64,
    pub clients: Vec<McpServerConnection>,
    pub tools: Vec<crate::types::tools::Tool>,
    pub commands: Vec<crate::commands::Command>,
    pub resources: std::collections::BTreeMap<String, Vec<ServerResource>>,
}

/// Maps to: CC AppStateStore.ts:185-217 inline plugins state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginsState {
    pub enabled: Vec<crate::types::plugin::LoadedPlugin>,
    pub disabled: Vec<crate::types::plugin::LoadedPlugin>,
    /// CC keeps the command array identity when other plugin state changes.
    pub commands: Arc<Vec<crate::commands::Command>>,
    pub errors: Vec<crate::types::plugin::PluginError>,
    pub installation_status: PluginInstallationStatus,
    pub needs_refresh: bool,
}
/// Maps to: CC AppStateStore.ts:196-209 installationStatus carrier.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginInstallationStatus {
    pub marketplaces: Vec<MarketplaceInstallationStatus>,
    pub plugins: Vec<PluginInstallationEntryStatus>,
}
/// Maps to: CC AppStateStore.ts:197-201 marketplace entry.
#[derive(Clone, Debug, PartialEq)]
pub struct MarketplaceInstallationStatus {
    pub name: String,
    pub status: String,
    pub error: Option<String>,
}
/// Maps to: CC AppStateStore.ts:202-208 plugin entry.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginInstallationEntryStatus {
    pub id: String,
    pub name: String,
    pub status: String,
    pub error: Option<String>,
}

/// AppStore-backed MCP reader/writer. Maps to: CC `AppState.mcp` consumption +
/// `useSetAppState()` writes. The data lives in `AppState.mcp`; obtain this in
/// components via `app_state::use_mcp` (or construct from an `AppStore` in
/// background tasks). NOT an independently provided context source.
#[derive(Clone)]
pub struct McpWriter {
    store: crate::state::store::AppStore,
}

impl McpWriter {
    pub fn new(store: crate::state::store::AppStore) -> Self {
        Self { store }
    }

    pub fn current(&self) -> McpState {
        (*self.store.get().mcp).clone()
    }

    /// Maps to: CC `useManageMCPConnections.ts:782-838` — the initialization
    /// `setAppState`. It MERGES newly-configured servers into the existing
    /// client list (`:833` `clients: [...mcpWithoutStale.clients,
    /// ...newClients]`) and returns `prevState` when there is nothing new and
    /// nothing stale (`:826-828`). Returns the stale clients for the caller's
    /// service-boundary cleanup, mirroring CC's `clearServerCache`
    /// fire-and-forget loop.
    ///
    /// Building the pending list from an empty state and overwriting the
    /// whole `AppState.mcp` instead would drop already-resolved capabilities,
    /// and a re-seed with identical configs would produce a revision bump, an
    /// `on_change`, and a listener pass the source does not produce — exactly
    /// what the strict/dynamic startup path does when its synchronous seed is
    /// followed by the async one.
    pub fn initialize_servers_as_pending(
        &self,
        configs: &indexmap::IndexMap<String, crate::services::mcp::types::ScopedMcpServerConfig>,
    ) -> Vec<McpServerConnection> {
        self.store.set_state(|prev| {
            let result =
                crate::services::mcp::use_manage_mcp_connections::initialize_servers_as_pending(
                    &prev.mcp, configs,
                );
            // Detach before publishing replacement pending state; an old
            // transport closing during async cache cleanup must not reconnect.
            for stale in &result.stale {
                if let Some(connected) = stale.as_connected() {
                    connected.client.clear_onclose();
                }
            }
            if result
                .stale
                .iter()
                .any(|server| server.connection_type() == McpServerConnectionType::Connected)
            {
                let cleanup =
                    crate::services::mcp::use_manage_mcp_connections::cleanup_stale_mcp_clients(
                        &result.stale,
                    );
                crate::utils::process_runtime::runtime_handle_for_detached_work()
                    .expect("MCP cleanup requires the process runtime")
                    .spawn(cleanup);
            }
            // CC :826-828 `if (newClients.length === 0 && stale.length === 0)
            // return prevState`.
            if result.new_clients.is_empty() && result.stale.is_empty() {
                return crate::state::store::UpdateDecision::Same(Vec::new());
            }
            let mut next = (**prev).clone();
            next.mcp = Arc::new(result.state);
            crate::state::store::UpdateDecision::Replace {
                next: Arc::new(next),
                result: result.stale,
            }
        })
    }

    /// Maps to: CC `useManageMCPConnections.ts#updateServer` callers that
    /// replace one MCP client snapshot plus its associated capabilities.
    pub fn apply_server_update(
        &self,
        server: impl Into<crate::services::mcp::use_manage_mcp_connections::McpPendingUpdate>,
    ) {
        self.store.replace_with(|app| {
            let mut state = (*app.mcp).clone();
            crate::services::mcp::use_manage_mcp_connections::apply_mcp_server_update(
                &mut state, server,
            );
            app.mcp = Arc::new(state);
        });
    }

    /// Maps to: CC `useManageMCPConnections.ts#flushPendingUpdates`
    /// (`:216-291`) — the batched counterpart of `updateServer`.
    ///
    /// At the source `updateServer` (`:297-308`) does NOT write the store: it
    /// pushes onto `pendingUpdatesRef` and arms a `MCP_BATCH_FLUSH_MS` (16ms)
    /// timer, and `flushPendingUpdates` then applies every queued patch inside
    /// ONE `setAppState`. So a startup that resolves N servers costs CC one
    /// transition per flush window, not one for every completed connection.
    ///
    /// Calling `apply_server_update` in a loop — which is what the resolved
    /// path did after d37dd99 — costs N revisions, N `on_change` runs and N
    /// listener passes instead. This entry restores the source's cost for
    /// callers that already hold a whole batch.
    ///
    /// The source hook's 16ms queue/timer is retained in
    /// `use_manage_mcp_connections::McpPendingUpdates`; this writer is the
    /// canonical flush reducer, and performs no connection or timer scheduling.
    pub fn apply_server_updates<
        T: Into<crate::services::mcp::use_manage_mcp_connections::McpPendingUpdate>,
    >(
        &self,
        servers: Vec<T>,
    ) {
        if servers.is_empty() {
            return;
        }
        self.store.replace_with(|app| {
            let mut state = (*app.mcp).clone();
            // CC `:224` `for (const update of updates)` inside the single
            // `setAppState`.
            for server in servers {
                crate::services::mcp::use_manage_mcp_connections::apply_mcp_server_update(
                    &mut state, server,
                );
            }
            app.mcp = Arc::new(state);
        });
    }

    /// Maps to: CC `setAppState(prev => ({ ...prev, mcp: ... }))` writers
    /// that rewrite `AppState.mcp` directly rather than through
    /// `updateServer` (`MCPRemoteServerMenu.tsx`'s clear-auth paths).
    pub fn update(&self, mutator: impl FnOnce(&mut McpState)) {
        self.store.replace_with(|app| {
            let mut state = (*app.mcp).clone();
            mutator(&mut state);
            app.mcp = Arc::new(state);
        });
    }

    /// Maps to an official client-only `updateServer` patch (for example a
    /// reconnecting `pending` status) that preserves existing capabilities.
    pub fn apply_client_update(&self, server: McpServerConnection) {
        self.store.replace_with(|app| {
            let mut state = (*app.mcp).clone();
            crate::services::mcp::use_manage_mcp_connections::apply_mcp_client_update(
                &mut state, server,
            );
            app.mcp = Arc::new(state);
        });
    }

    /// Maps to: CC `registerElicitationHandler` → `setAppState` queue push.
    pub fn push_elicitation_event(
        &self,
        event: crate::services::mcp::elicitation_handler::ElicitationRequestEvent,
    ) {
        self.store.replace_with(|app| {
            let mut state = (*app.elicitation).clone();
            state.push_event(event);
            app.elicitation = Arc::new(state);
        });
    }

    /// A cancelled request leaves the dialog queue ([`ElicitationState::remove`]).
    pub fn remove_elicitation_event(&self, server_name: &str, request_id: &str) {
        self.store.set_state(|prev| {
            let mut state = (*prev.elicitation).clone();
            if !state.remove(server_name, request_id) {
                return crate::state::store::UpdateDecision::Same(());
            }
            let mut next = (**prev).clone();
            next.elicitation = Arc::new(state);
            crate::state::store::UpdateDecision::Replace {
                next: Arc::new(next),
                result: (),
            }
        });
    }

    /// Maps to: CC elicitation-complete notification → `setAppState` queue update.
    ///
    /// B3 flip-audit: CC elicitationHandler.ts:194 — `if (idx === -1) return
    /// prev`, with the `found` flag driving the not-found debug log
    /// (:200-205).
    pub fn mark_elicitation_complete(&self, name: &str, elicitation_id: &str) {
        let found = self.store.set_state(|prev| {
            let mut state = (*prev.elicitation).clone();
            if !state.mark_complete(name, elicitation_id) {
                return crate::state::store::UpdateDecision::Same(false);
            }
            let mut next = (**prev).clone();
            next.elicitation = Arc::new(state);
            crate::state::store::UpdateDecision::Replace {
                next: Arc::new(next),
                result: true,
            }
        });
        if !found {
            crate::utils::log::log_mcp_debug(
                name,
                &format!(
                    "Ignoring completion notification for unknown elicitation: {elicitation_id}"
                ),
            );
        }
    }
}

/// Maps to: CC `AppStateStore.ts` `AppState` (incremental subset).
///
/// `PartialEq` is KEPT post-B3-flip (Contract A clause-1 amendment): the
/// store layer never compares values (root identity is `Arc::ptr_eq` only);
/// consumers are source-branch guards (file_history/denial/channel etc.
/// projecting CC `return prev` predicates) and tests.
#[derive(Clone, Debug, PartialEq)]
pub struct AppState {
    /// Maps to: CC `AppStateStore.ts:401` / `getDefaultAppState:563`.
    pub auth_version: u64,
    /// Maps to: CC `AppState.verbose` — persisted to globalConfig on change.
    pub verbose: bool,
    /// Nyan extension (no CC counterpart) — user-authorized L2 (v3 ruling,
    /// 2026-08-01); follows the CC verbose pattern (AppStateStore.ts:91).
    /// Single-source expansion control for thinking blocks, REPLACING CC's
    /// verbose gate at the leaf (AssistantThinkingMessage.tsx:38). Default
    /// true — mirrors the authoritative factory default (`expandThinking`,
    /// config.rs `create_official_default_global_config`).
    pub expand_thinking: bool,
    /// Nyan extension (no CC counterpart) — user-authorized L2 (v3 ruling,
    /// 2026-08-01); follows the CC verbose pattern (AppStateStore.ts:91).
    /// Single-source expansion control for collapsed read/search groups,
    /// REPLACING CC's verbose gate at the leaf
    /// (CollapsedReadSearchContent.tsx:36-43). Default true — mirrors the
    /// authoritative factory default (`expandCollapsedReadSearch`,
    /// config.rs `create_official_default_global_config`).
    pub expand_collapsed_read_search: bool,
    /// The profile this session's main loop runs on (`--model`, `/model`, the
    /// picker, a resumed session); `None` is `selection.main`. Never persisted
    /// to settings: the picker's Ctrl+S writes `selection.main` itself.
    ///
    /// @nyan: replaces CC `AppState.mainLoopModel` /
    /// `mainLoopModelForSession`. The wire model is the profile's.
    pub main_loop_profile: Option<String>,
    /// Maps to: CC `AppState.advisorModel` — session-visible advisor model,
    /// persisted separately by `/advisor` to user settings when configured.
    pub advisor_model: Option<String>,
    /// Maps to: CC `AppState.effortValue`.
    pub effort_value: Option<crate::utils::effort::Effort>,
    /// CC cacheMissAckedAtOutputTokens: confirmation watermark.
    pub cache_miss_acked_at_output_tokens: i64,
    /// Maps to: CC `AppState.expandedView` — persisted as the legacy
    /// `showExpandedTodos`/`showSpinnerTree` globalConfig pair.
    pub expanded_view: ExpandedView,
    /// Maps to: CC `AppState.settings` (AppStateStore.ts:90) — the merged
    /// settings snapshot, replaced wholesale by `apply_settings_change` when
    /// settings files change on disk (CC `applySettingsChange`).
    pub settings: Arc<crate::utils::settings::SettingsJson>,
    /// Maps to: CC `AppState.toolPermissionContext` — the most central
    /// field; mode transitions fan out through `on_change_app_state`.
    /// Arc-wrapped: rule maps make it the heaviest field in this subset.
    pub tool_permission_context: Arc<ToolPermissionContext>,
    /// Maps to: CC `AppState.denialTracking` (denialTracking.ts) — auto-mode
    /// classifier consecutive/total denial counters.
    /// Value retention (no Arc): Copy-sized counter struct — value projection
    /// is equivalent to CC's reference projection (B3 review A-7,
    /// permissions.ts:974 + denialTracking.ts:33).
    pub denial_tracking: Option<crate::utils::permissions::denial_tracking::DenialTrackingState>,
    /// Maps to: CC AppStateStore.ts:94 statusLineText. Plain
    /// `string | undefined` — configured/padding are derived from
    /// `AppState.settings` at the read sites (CC StatusLine.tsx:59/:391).
    /// Written by the async statusline-command future at App mount; read
    /// by the prompt footer.
    pub status_line_text: Option<String>,
    /// Maps to: CC `AppState.notifications` — `{current, queue}` consumed by
    /// the prompt-input notification row.
    pub notifications: Arc<crate::context::notifications::NotificationsState>,
    /// Maps to: CC `AppState.elicitation` — MCP elicitation request queue.
    pub elicitation: Arc<ElicitationState>,
    /// Maps to: CC `AppState.mcp` — `{clients, tools, commands, resources}`
    /// per-server aggregate, populated asynchronously by the connection
    /// manager without blocking first paint.
    pub mcp: Arc<McpState>,
    /// Maps to: CC AppStateStore.ts:185-217; shared plugin UI/runtime state.
    pub plugins: Arc<PluginsState>,
    /// Maps to: CC `AppState.channelPermissionCallbacks` — populated by the
    /// connection manager (official gates), read by the interactive tool
    /// permission handler.
    pub channel_permission_callbacks:
        Option<crate::services::mcp::channel_permissions::ChannelPermissionCallbacks>,
    /// Maps to: CC `AppState.activeOverlays: ReadonlySet<string>` — open
    /// overlay ids for Escape-key coordination (context/overlayContext.tsx).
    pub active_overlays: Arc<std::collections::BTreeSet<String>>,
    /// Maps to: CC `AppState.fileHistory` (utils/fileHistory.ts
    /// FileHistoryState) — file-checkpointing snapshots behind /rewind.
    /// Arc-wrapped: snapshots hold per-file backup maps.
    pub file_history: Arc<crate::utils::file_history::FileHistoryState>,
    /// Maps to: CC `AppState.attribution` (utils/commitAttribution.ts).
    /// P4 identity: Arc mirrors JS reference semantics — CC
    /// `updateAttributionState` (REPL.tsx:3229-3236) early-returns when
    /// `updated === prev.attribution`, so a wired Rust write site must guard
    /// with `Arc::ptr_eq` (the counter write sites are currently unwired; an
    /// existing seam orthogonal to the Arc migration). Inner `file_states`
    /// stays by-value: the high-frequency writer is ant-only and unwired, so
    /// `Arc::make_mut`'s O(files) copy bound is acceptable (seam).
    pub attribution: Arc<crate::utils::commit_attribution::AttributionState>,
    /// Maps to: CC `AppState.selectedIPAgentIndex` — `-1` = leader/pill,
    /// `0..N-1` = teammate rows, `N` = hide row while selecting.
    pub selected_ip_agent_index: i32,
    /// Maps to: CC `AppState.coordinatorTaskIndex` — `-1` = pill, `0` = main,
    /// `1..N` = agent rows in CoordinatorTaskPanel.
    pub coordinator_task_index: i32,
    /// Maps to: CC `AppState.viewSelectionMode`.
    pub view_selection_mode: ViewSelectionMode,
    /// Maps to: CC `AppState.viewingAgentTaskId` — in-process teammate whose
    /// transcript is foregrounded (`None` = leader view).
    pub viewing_agent_task_id: Option<String>,
    /// Maps to: CC `AppState.showTeammateMessagePreview`.
    pub show_teammate_message_preview: bool,
    /// Maps to: CC `AppState.teamContext` — swarm team identity + teammate map.
    /// Written by inbox poller / team-file hydrate; read by Footer Teams pill,
    /// SwarmBanner, and permission routing.
    /// P4 identity: Arc per JS whole-object write — the wholesale replacement
    /// write points are TeamCreateTool.ts:196, spawnMultiAgent.ts:454/667/966,
    /// and TeamDeleteTool.ts:120; the only nested spread is
    /// useInboxPoller.ts:770-772 (rebuilds `teammates` wholesale, no
    /// per-teammate identity consumer → single-layer Arc suffices). The REPL
    /// poll loop rebuilds the value each tick, so the write-back guard
    /// compares by value, never `Arc::ptr_eq`.
    pub team_context: Option<Arc<crate::hooks::use_inbox_poller::InboxPollerTeamContext>>,
    /// Maps to: CC `AppState.agent` — resolved main-thread agent type for logo
    /// and swarm banner (not the in-process subagent id).
    pub agent: Option<String>,
    /// Maps to: CC `AppState.agentDefinitions`. Startup resolves this once and
    /// resume may replace it after a coordinator-mode switch; REPL never
    /// re-reads agent files from its render/query paths.
    pub agent_definitions: Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult>,
    /// Maps to: CC `AppState.isBriefOnly`.
    pub is_brief_only: bool,
    /// Maps to: CC `AppState.promptSuggestionEnabled` — session gate for
    /// placeholder / suggestion generation (seeded from settings, toggled via
    /// Config preview like verbose).
    pub prompt_suggestion_enabled: bool,
    /// Maps to: CC `AppState.promptSuggestion` — generated suggestion payload.
    /// Value retention (no Arc): small struct; every write site carries a
    /// source-branch value guard, so value projection is equivalent to CC's
    /// reference projection (same rationale as `denial_tracking`).
    pub prompt_suggestion: PromptSuggestionState,
    /// Maps to: CC `AppState.footerSelection` — focused footer pill, or
    /// `None` when the prompt owns focus.
    pub footer_selection: Option<FooterItem>,
    // -- Always-on bridge (CC AppStateStore.ts:134-155) --
    // The CC `feature('BRIDGE_MODE')` build gate and `isBridgeEnabled()`
    // entitlement are NOT AppState (CC evaluates them at the read sites:
    // PromptInputFooter.tsx:241/:255); Nyan mirrors that with
    // `FeatureFlag::BridgeMode` + `bridge::bridge_enabled::is_bridge_enabled`.
    // Truthful seam: no Nyan producer ever sets `repl_bridge_enabled` true
    // (CC's settings-screen bridge toggle and useReplBridge transport are
    // unported); only the BridgeDialog disconnect writes `false`, so the
    // footer pill stays unreachable in production even when both live gates
    // pass. `repl_bridge_outbound_only` / `repl_bridge_initial_name` are
    // additionally read-never here (CC readers live in the unported bridge
    // runtime); the URL/id/error fields are write-never but read by
    // BridgeDialogSnapshot.
    /// Maps to: CC AppStateStore.ts:134 `replBridgeEnabled`.
    pub repl_bridge_enabled: bool,
    /// Maps to: CC AppStateStore.ts:136 `replBridgeExplicit`.
    pub repl_bridge_explicit: bool,
    /// Maps to: CC AppStateStore.ts:138 `replBridgeOutboundOnly`.
    pub repl_bridge_outbound_only: bool,
    /// Maps to: CC AppStateStore.ts:140 `replBridgeConnected`.
    pub repl_bridge_connected: bool,
    /// Maps to: CC AppStateStore.ts:142 `replBridgeSessionActive`.
    pub repl_bridge_session_active: bool,
    /// Maps to: CC AppStateStore.ts:144 `replBridgeReconnecting`.
    pub repl_bridge_reconnecting: bool,
    /// Maps to: CC AppStateStore.ts:146 `replBridgeConnectUrl`.
    pub repl_bridge_connect_url: Option<String>,
    /// Maps to: CC AppStateStore.ts:148 `replBridgeSessionUrl`.
    pub repl_bridge_session_url: Option<String>,
    /// Maps to: CC AppStateStore.ts:150 `replBridgeEnvironmentId`.
    pub repl_bridge_environment_id: Option<String>,
    /// Maps to: CC AppStateStore.ts:151 `replBridgeSessionId`.
    pub repl_bridge_session_id: Option<String>,
    /// Maps to: CC AppStateStore.ts:153 `replBridgeError`.
    pub repl_bridge_error: Option<String>,
    /// Maps to: CC AppStateStore.ts:155 `replBridgeInitialName`.
    pub repl_bridge_initial_name: Option<String>,
    /// Maps to: CC AppStateStore.ts:157 `showRemoteCallout` — top-level
    /// sibling of the replBridge* fields, not nested under them. Seeded from
    /// the startup RemoteCallout snapshot; dismissed by the callout dialog.
    pub show_remote_callout: bool,
    /// Maps to: CC `AppState.standaloneAgentContext` — non-swarm custom
    /// name/color for the swarm banner (resume / agent metadata).
    /// Value retention (no Arc): two-field small object; all write sites
    /// replace it wholesale.
    pub standalone_agent_context:
        Option<crate::utils::session_restore::RestoredStandaloneAgentContext>,
    /// Maps to: CC `AppState.foregroundedTaskId` — local-agent task whose
    /// messages are shown in the main view (`None` = no foreground override).
    pub foregrounded_task_id: Option<String>,
    /// Maps to: CC `AppState.inbox`.
    /// P4 identity: Arc — the poll core (`InboxPollMutable`) stays by-value and
    /// the REPL wraps it in a fresh Arc only at the write-back boundary.
    pub inbox: Arc<crate::hooks::use_inbox_poller::InboxState>,
    /// Maps to: CC `AppState.workerSandboxPermissions`.
    /// P4 identity: single-layer Arc — the nested-spread write points are
    /// REPL.tsx:6450-6456 (`queue.slice(1)` dequeue) and
    /// useInboxPoller.ts:440-449 (queue append); both rebuild `queue` and
    /// only carry the scalar fields forward, and `queue` has no identity
    /// consumer, so no inner Arc. (AppStateStore.ts:363-373 is the shape
    /// declaration only, not a write point.)
    pub worker_sandbox_permissions:
        Arc<crate::hooks::use_inbox_poller::WorkerSandboxPermissionsState>,
    /// Maps to: CC `AppState.pendingWorkerRequest` (worker waiting on leader).
    /// P4 identity: Arc — CC writes the whole object
    /// (swarmWorkerHandler.ts:62-65/:126-133).
    pub pending_worker_request: Option<Arc<crate::hooks::use_inbox_poller::PendingWorkerRequest>>,
    /// Maps to: CC `AppState.pendingSandboxRequest` (worker waiting on leader).
    /// P4 identity: Arc — CC writes the whole object (REPL.tsx:2966-2973;
    /// useInboxPoller.ts:488-491 is the clear-to-null write).
    pub pending_sandbox_request: Option<Arc<crate::hooks::use_inbox_poller::PendingSandboxRequest>>,
    /// Maps to: CC `AppState.spinnerTip` — tip text shown under the spinner
    /// while a turn is loading (`None` until tip scheduler picks one).
    pub spinner_tip: Option<String>,
    /// Maps to: CC `AppState.todos` — agentId → TodoList. Empty until
    /// TodoWrite / session restore hydrates.
    /// P4 identity: single-layer Arc — per-list identity has no consumer: CC
    /// sessionRestore.ts:138-140 states interactive mode uses the file-backed
    /// v2 store and `AppState.todos` only serves SDK/non-interactive callers,
    /// and Nyan has zero render-path consumers of this map.
    pub todos: Arc<std::collections::BTreeMap<String, Vec<TodoItem>>>,
    /// Maps to: CC `AppState.tasks` — taskId → TaskState. In-process
    /// teammates use [`TaskState::InProcessTeammate`]; other types are
    /// [`TaskState::Other`] stubs until registries merge.
    /// P4 identity (2026-08-02): double-layer Arc — CC writes
    /// `{...prev, tasks: {...prev.tasks, [id]: task}}` at every mutation
    /// (utils/task/framework.ts:64-70, stopTask.ts:78-84,
    /// LocalMainSessionTask.ts:290-298), i.e. a fresh map reference AND a
    /// fresh per-task reference per write. The outer Arc carries the map
    /// identity, the inner Arc the per-task identity, which makes CC's
    /// `updated === task` reference guard (framework.ts:59) expressible as
    /// `Arc::ptr_eq` in `update_task_state`.
    pub tasks: Arc<std::collections::BTreeMap<String, Arc<TaskState>>>,
    // Footer layout wakes are NOT AppState: row-visibility flips bump the
    // PromptInput-scoped FooterLayoutWake (L1, PORTING.md;
    // components/prompt_input/footer_layout_wake.rs), replacing the deleted
    // `footer_layout_epoch` whole-tree wake. Height is recomputed live (no
    // mirrored metrics bag); auth is a first-class field; tokenUsage,
    // ideSelection, and the auto-updater values are component-local (CC
    // Notifications props), threaded to the height budget by their
    // REPL/PromptInput owners.
    /// Maps to: CC `AppStateStore.ts:445` `isUltraplanMode?: boolean` — the
    /// remote-harness side, set via the `set_permission_mode` control request
    /// and pushed to CCR `external_metadata.is_ultraplan_mode` by
    /// `onChangeAppState`. Optional in CC with no `getDefaultAppState` entry,
    /// hence `Option<bool>` defaulting to `None` rather than `false`.
    pub is_ultraplan_mode: Option<bool>,
}

impl Default for AppState {
    /// Maps to: CC `getDefaultAppState()` (AppStateStore.ts:456-569) for the
    /// covered subset: `verbose: false`, `mainLoopModel: null`,
    /// `expandedView: 'none'`, default permission context. Dynamic defaults
    /// (settings, thinkingEnabled, promptSuggestionEnabled) are restored by
    /// `main.rs` `build_initial_app_state_with_thinking`.
    ///
    /// NOT yet ported (2026-08-02 2nd-addendum audit; MODULE_MAP row is
    /// partial): the teammate plan-mode-required initial mode
    /// (AppStateStore.ts:457-466 + main.tsx:4028-4034) — no caller applies it
    /// today — and the agentNameRegistry / plugins / sessionHooks /
    /// initialMessage fields (trivial
    /// constants in CC whose subsystems are unported).
    fn default() -> Self {
        Self {
            auth_version: 0,
            verbose: false,
            expand_thinking: true,
            expand_collapsed_read_search: true,
            main_loop_profile: None,
            advisor_model: None,
            effort_value: None,
            cache_miss_acked_at_output_tokens: -1,
            expanded_view: ExpandedView::None,
            settings: Arc::new(crate::utils::settings::SettingsJson::default()),
            tool_permission_context: Arc::new(ToolPermissionContext::default()),
            denial_tracking: None,
            // Maps to: CC AppStateStore.ts:475 `statusLineText: undefined`.
            status_line_text: None,
            notifications: Arc::new(crate::context::notifications::NotificationsState::default()),
            elicitation: Arc::new(ElicitationState::default()),
            mcp: Arc::new(McpState::default()),
            plugins: Arc::new(PluginsState::default()),
            channel_permission_callbacks: None,
            active_overlays: Arc::new(std::collections::BTreeSet::new()),
            file_history: Arc::new(crate::utils::file_history::FileHistoryState::default()),
            attribution: Arc::new(
                crate::utils::commit_attribution::create_empty_attribution_state(),
            ),
            selected_ip_agent_index: -1,
            coordinator_task_index: -1,
            view_selection_mode: ViewSelectionMode::None,
            viewing_agent_task_id: None,
            show_teammate_message_preview: false,
            team_context: None,
            agent: None,
            agent_definitions: Arc::new(
                crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default(),
            ),
            is_brief_only: false,
            prompt_suggestion_enabled: false,
            prompt_suggestion: PromptSuggestionState::default(),
            footer_selection: None,
            // Maps to: CC AppStateStore.ts:487-499 replBridge* /
            // showRemoteCallout defaults (all false / undefined).
            repl_bridge_enabled: false,
            repl_bridge_explicit: false,
            repl_bridge_outbound_only: false,
            repl_bridge_connected: false,
            repl_bridge_session_active: false,
            repl_bridge_reconnecting: false,
            repl_bridge_connect_url: None,
            repl_bridge_session_url: None,
            repl_bridge_environment_id: None,
            repl_bridge_session_id: None,
            repl_bridge_error: None,
            repl_bridge_initial_name: None,
            show_remote_callout: false,
            standalone_agent_context: None,
            foregrounded_task_id: None,
            inbox: Arc::new(crate::hooks::use_inbox_poller::InboxState::default()),
            worker_sandbox_permissions: Arc::new(
                crate::hooks::use_inbox_poller::WorkerSandboxPermissionsState::default(),
            ),
            pending_worker_request: None,
            pending_sandbox_request: None,
            spinner_tip: None,
            todos: Arc::new(std::collections::BTreeMap::new()),
            tasks: Arc::new(std::collections::BTreeMap::new()),
            // CC `AppStateStore.ts:445` is optional with no default entry.
            is_ultraplan_mode: None,
        }
    }
}

impl AppState {
    /// Convenience for the dominant mutation shape: replace the permission
    /// context wholesale (CC spreads a new object into the field).
    pub fn set_tool_permission_context(&mut self, context: ToolPermissionContext) {
        self.tool_permission_context = Arc::new(context);
    }
}

#[cfg(test)]
mod tests {
    use super::{AppState, McpState, McpWriter};
    use crate::services::mcp::types::{
        McpServerConnection, McpServerConnectionType, ServerResource,
    };
    use std::sync::Arc;

    fn stdio_scoped_config(command: &str) -> crate::services::mcp::types::ScopedMcpServerConfig {
        crate::services::mcp::types::ScopedMcpServerConfig {
            name: None,
            scope: crate::services::mcp::types::ConfigScope::Project,
            transport: crate::services::mcp::types::Transport::Stdio,
            command: Some(command.to_string()),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            url: None,
            headers: std::collections::BTreeMap::new(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            plugin_source: None,
        }
    }

    /// Maps to: CC `useManageMCPConnections.ts#flushPendingUpdates`
    /// (`:216-291`) — N resolved servers cost ONE store transition, because
    /// `updateServer` (`:297-308`) only queues and the 16ms flush applies the
    /// whole batch inside a single `setAppState`.
    ///
    /// Looping `apply_server_update` instead costs N revisions, N `on_change`
    /// runs and N listener passes.
    #[test]
    fn resolved_servers_apply_in_one_transition_like_official_flush() {
        let store = crate::state::store::AppStore::new(AppState::default(), None);
        let writer = McpWriter::new(store.clone());
        // A resolved server's connection attempt.
        let server = |name: &str| {
            crate::services::mcp::client::McpConnectionDiscovery::from(
                McpServerConnection::connected_for_test(name, stdio_scoped_config(name)),
            )
        };

        let baseline = store.revision();
        let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notified_in_listener = notified.clone();
        store.subscribe(Arc::new(move || {
            notified_in_listener.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));

        writer.apply_server_updates(vec![server("docs"), server("ide"), server("search")]);

        assert_eq!(
            store.revision() - baseline,
            1,
            "three resolved servers must be one transition, as CC's flush is"
        );
        assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(writer.current().clients.len(), 3);

        // CC `:219` `if (updates.length === 0) return` — an empty flush is not
        // a transition at all.
        let after = store.revision();
        writer.apply_server_updates(
            Vec::<crate::services::mcp::client::McpConnectionDiscovery>::new(),
        );
        assert_eq!(store.revision(), after);
    }

    /// Maps to: CC `useManageMCPConnections.ts:826-828` — `if
    /// (newClients.length === 0 && stale.length === 0) return prevState`.
    ///
    /// The strict/dynamic startup path seeds pending state synchronously and
    /// then the async path seeds again with the same configs. An
    /// unconditional write would make that second seed produce a revision
    /// bump, an `on_change` and a listener pass the source never produces.
    #[test]
    fn reseeding_identical_configs_is_a_same_like_official_initialization_guard() {
        let store = crate::state::store::AppStore::new(AppState::default(), None);
        let writer = McpWriter::new(store.clone());
        let configs =
            indexmap::IndexMap::from([("docs".to_string(), stdio_scoped_config("docs-mcp"))]);

        let stale = writer.initialize_servers_as_pending(&configs);
        assert!(stale.is_empty());
        let after_first = store.revision();
        assert_eq!(after_first, 1, "the first seed installs a root");
        assert_eq!(writer.current().clients.len(), 1);

        let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notified_in_listener = notified.clone();
        store.subscribe(Arc::new(move || {
            notified_in_listener.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));

        let stale = writer.initialize_servers_as_pending(&configs);
        assert!(stale.is_empty());
        assert_eq!(
            store.revision(),
            after_first,
            "nothing new and nothing stale must be a Same (CC :826-828)"
        );
        assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Maps to: CC `:833` `clients: [...mcpWithoutStale.clients,
    /// ...newClients]` — initialization MERGES; it does not replace. A server
    /// that already resolved keeps its capabilities when a later seed adds a
    /// different server.
    #[test]
    fn initialization_merges_into_resolved_clients_like_official() {
        let store = crate::state::store::AppStore::new(AppState::default(), None);
        let writer = McpWriter::new(store.clone());
        let config = stdio_scoped_config;

        writer.initialize_servers_as_pending(&indexmap::IndexMap::from([(
            "docs".to_string(),
            config("docs-mcp"),
        )]));
        // "docs" resolves with a tool.
        writer.apply_server_update(
            crate::services::mcp::use_manage_mcp_connections::McpPendingUpdate {
                // Carry the config forward: `exclude_stale_plugin_clients`
                // hashes it, and a mismatch legitimately marks the server
                // stale, which CC re-adds as `pending` (:786-789). This test
                // is about the merge, not the stale path.
                server: McpServerConnection::connected_for_test(
                    "docs",
                    stdio_scoped_config("docs-mcp"),
                ),
                tools: Some(vec![crate::services::mcp::client::mcp_tool_for_test(
                    "docs",
                    serde_json::json!({
                        "name": "search",
                        "description": "Search docs",
                        "inputSchema": {"type": "object"},
                        "annotations": {"readOnlyHint": true}
                    }),
                )]),
                commands: None,
                resources: None,
            },
        );
        assert_eq!(writer.current().tools.len(), 1);

        // A later seed that adds a DIFFERENT server must not wipe "docs".
        writer.initialize_servers_as_pending(&indexmap::IndexMap::from([
            ("docs".to_string(), config("docs-mcp")),
            ("ide".to_string(), config("ide-mcp")),
        ]));

        let state = writer.current();
        assert_eq!(state.clients.len(), 2);
        let docs = state
            .clients
            .iter()
            .find(|server| server.name() == "docs")
            .expect("docs client survives");
        assert_eq!(docs.connection_type(), McpServerConnectionType::Connected);
        assert_eq!(
            state.tools.len(),
            1,
            "already-resolved capabilities survive a later seed (CC merges)"
        );
    }

    #[test]
    fn mcp_writer_update_preserves_ready_flat_aggregates_without_reprojection() {
        let store = crate::state::store::AppStore::new(AppState::default(), None);
        let writer = McpWriter::new(store);
        let state = McpState {
            tools: vec![crate::types::tools::Tool {
                name: "mcp__docs__search".to_string(),
                description: "Search docs".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                is_mcp: true,
                ..Default::default()
            }],
            commands: vec![crate::services::mcp::client::mcp_command_for_test(
                "docs",
                "summarize",
                Some("Summarize docs"),
                &[],
            )],
            resources: std::collections::BTreeMap::from([(
                "docs".to_string(),
                vec![ServerResource {
                    server: "docs".to_string(),
                    uri: "docs://guide".to_string(),
                    name: "guide".to_string(),
                    description: None,
                    mime_type: None,
                }],
            )]),
            ..McpState::default()
        };

        writer.update(|mcp| {
            mcp.tools = state.tools;
            mcp.commands = state.commands;
            mcp.resources = state.resources;
        });
        let current = writer.current();
        assert_eq!(current.tools[0].name, "mcp__docs__search");
        assert_eq!(current.commands[0].name, "mcp__docs__summarize");
        assert_eq!(current.resources["docs"][0].uri, "docs://guide");
    }

    #[test]
    fn display_pref_defaults_mirror_factory_true() {
        // Ruled default (v3, 2026-08-01): both Nyan display prefs default
        // true, mirroring the authoritative factory defaults
        // (`create_official_default_global_config`).
        assert!(AppState::default().expand_thinking);
        assert!(AppState::default().expand_collapsed_read_search);
    }
}
