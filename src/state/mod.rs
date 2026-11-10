//! Maps to: CC state/ — application state management.
//!
//! **Consumers: read `docs/STATE_STORE_USAGE.md` first.** It is the integration
//! standard — how to read, how to write, how to add a field, and the known
//! deviations that are easy to trip over. This header explains the layout; that
//! document explains the usage.
//!
//! This file declares no items of its own, which is the point: CC's `state/`
//! has no module-level implementation either. Everything that had accumulated
//! here now lives with the CC file that owns it (#33) — MCP connection
//! records in `services/mcp/types.rs` (CC `services/mcp/types.ts`),
//! `McpState`/`McpWriter` in `app_state_store` (CC declares the bag inline at
//! `AppStateStore.ts:173`), the Status-only diagnostics in `utils/status.rs`
//! with its `StartupDiagnosticsSnapshot`.
//!
//! Unified state loop (CC parity) — one Rust file per CC file, named after the
//! CC file it maps to, not after the type it happens to define:
//!   - `store` — framework-agnostic external store (CC state/store.ts)
//!   - `app_state_store` — the AppState shape (CC state/AppStateStore.ts)
//!   - `on_change_app_state` — diff-driven side-effect choke point
//!     (CC state/onChangeAppState.ts)
//!   - `app_state` — AppStateProvider + the useAppState family
//!     (CC state/AppState.tsx)
//! Component-local transient state stays in per-component `use_state`, same
//! boundary as CC's REPL local useState vs AppState split.
//!
//! Truth lives only in `AppState`. Components read via
//! `app_state::use_app_state`; writes go through `AppStore::set_state` /
//! `replace_with`, either
//! directly (CC: bare `setAppState`, e.g. elicitation) or via a writer handle
//! obtained from a hook (`NotificationsWriter` ≙ CC `useNotifications()`
//! closures, `McpWriter` ≙ CC `updateServer`). statusline/elicitation/
//! notifications/mcp/config/
//! footer_selection/view_selection_mode/
//! selected_ip_agent_index/coordinator_task_index/
//! viewing_agent_task_id/show_teammate_message_preview/team_context/agent/
//! agent_definitions/
//! spinner_tip/todos/tasks/standalone_agent_context/foregrounded_task_id/
//! inbox/worker_sandbox_permissions/pending_worker_request/
//! pending_sandbox_request are fully absorbed (settings live in
//! `AppState.settings` + watcher; globalConfig reads go through the cached
//! `load_global_config()`; startup diagnostics are a plain injected
//! `StartupDiagnosticsSnapshot`). Footer Notification rows use live height
//! + the PromptInput-scoped `FooterLayoutWake` (L1, PORTING.md; layout flips
//! never wake the AppStore — no FooterIndicators bag);
//! prefersReducedMotion is read from
//! `AppState.settings` directly (no DisplaySettings bag);
//! teammate tree selection/focus mirrors CC `selectedIPAgentIndex` /
//! `viewSelectionMode` / `viewingAgentTaskId`; spinner tree visibility is
//! `expandedView == Teammates`; in-process teammate rows live in
//! `AppState.tasks` (`TaskState::InProcessTeammate`). Selectors live in
//! `state/selectors.rs`. New shared state goes into `AppState` — never a
//! new independently provided context source.

pub mod app_state;
pub mod app_state_store;
pub mod on_change_app_state;
pub mod selectors;
pub mod store;
pub mod teammate_view_helpers;
