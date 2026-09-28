//! Maps to: CC `hooks/useCancelRequest.ts` — the CancelRequestHandler.
//!
//! CC renders a null component that registers `chat:cancel` (Escape) and
//! `app:interrupt` (CC's Ctrl+C) keybinding handlers with independent isActive
//! gates. Cometix keeps the same action names; `CancelRequestHandler` below is
//! that component, which REPL mounts ahead of PromptInput as CC does
//! (REPL.tsx:5891, :6133).
//!
//! Event-order note: Ink hands a key to listeners in registration order, and
//! this handler registered before PromptInput's TextInput, so it wins Escape
//! while a task runs. iocraft polls earlier siblings first, which puts this
//! component ahead of PromptInput the same way; the input layer's
//! `UseTextInputOptions::cancel_passthrough` also leaves Escape unconsumed.
//!
//! Coverage vs CC (useCancelRequest.ts):
//! - Priority 1 (:97-103): active task → clear permission queue + on_cancel ✓
//! - Priority 2 (:106-111): pop queued command — command queue does not
//!   exist in Cometix yet; seam documented in options.
//! - isEscapeActive (:129-154): context guards are passed in by Repl as a
//!   single `is_context_blocked` flag (CC receives the same facts as props);
//!   overlay gate reads `context::overlay_context::is_overlay_active` ✓; the
//!   special-mode-empty-input and teammate-view exclusions join when those
//!   states are lifted out of PromptInput/swarm.
//! - `app:interrupt` is CC's Ctrl+C. Cometix binds Ctrl+C to `app:exit`
//!   (2.0.x semantics, `default_bindings.rs`), so the interrupt leg answers
//!   only a key a user binds to it; Ctrl+C stays PromptInput's text-level
//!   clear and double-press exit, running turn or not.

use iocraft::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::state::store::AppStore;

pub struct UseCancelRequestOptions<CanCancel, OnCancel>
where
    CanCancel: Fn() -> bool + Send + 'static,
    OnCancel: Fn() + Send + 'static,
{
    pub app_store: AppStore,
    /// Maps to: CC `abortSignal !== undefined && !abortSignal.aborted`.
    pub can_cancel_running_task: CanCancel,
    /// Maps to: CC `onCancel` — clears the tool-use confirm queue and aborts
    /// the active query (REPL owns both, same as CC's REPL-provided props).
    pub on_cancel: OnCancel,
    /// Maps to: CC context guards (:141-148) — transcript screen, history
    /// search, message selector, local JSX command, help. Repl computes and
    /// passes the aggregate, mirroring CC's prop-driven inputs.
    pub is_context_blocked: bool,
}

/// Maps to: CC `CancelRequestHandler` mounting + the
/// `useKeybinding('chat:cancel', handleCancel, {isActive})` registration.
pub fn use_cancel_request<CanCancel, OnCancel>(
    hooks: &mut Hooks,
    options: UseCancelRequestOptions<CanCancel, OnCancel>,
) where
    CanCancel: Fn() -> bool + Send + 'static,
    OnCancel: Fn() + Send + 'static,
{
    let UseCancelRequestOptions {
        app_store,
        can_cancel_running_task,
        on_cancel,
        is_context_blocked,
    } = options;

    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let kill_agents_shortcut = runtime.as_ref().map_or_else(
        || "ctrl+x ctrl+k".to_string(),
        |runtime| {
            crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
                "chat:killAgents",
                &ContextName::Chat,
                "ctrl+x ctrl+k",
                runtime.bindings().as_slice(),
            )
        },
    );
    let notifications = crate::context::notifications::use_notifications(hooks);
    let mut last_kill_agents_press = hooks.use_state(|| Option::<Instant>::None);
    let can_cancel_running_task = Arc::new(Mutex::new(can_cancel_running_task));
    let on_cancel = Arc::new(Mutex::new(on_cancel));

    // Maps to: CC `useKeybinding('chat:cancel', handleCancel, ...)`.
    use_keybinding(
        hooks,
        runtime.clone(),
        "chat:cancel",
        ContextName::Chat,
        {
            let app_store = app_store.clone();
            let can_cancel_running_task = Arc::clone(&can_cancel_running_task);
            move || {
                !is_context_blocked
                    && !crate::context::overlay_context::is_overlay_active(&app_store)
                    && can_cancel_running_task
                        .lock()
                        .expect("cancel predicate lock")()
            }
        },
        {
            let on_cancel = Arc::clone(&on_cancel);
            move || {
                on_cancel.lock().expect("cancel handler lock")();
                true
            }
        },
    );

    // Maps to: CC `useKeybinding('app:interrupt', handleInterrupt, ...)`.
    // Teammate-view and queued-command branches remain explicit task/queue
    // seams; the current owner claims this action only for a live query.
    use_keybinding(
        hooks,
        runtime.clone(),
        "app:interrupt",
        ContextName::Global,
        {
            let app_store = app_store;
            let can_cancel_running_task = Arc::clone(&can_cancel_running_task);
            move || {
                !is_context_blocked
                    && !crate::context::overlay_context::is_overlay_active(&app_store)
                    && can_cancel_running_task
                        .lock()
                        .expect("cancel predicate lock")()
            }
        },
        move || {
            on_cancel.lock().expect("cancel handler lock")();
            true
        },
    );

    // Maps to: CC `chat:killAgents`' always-registered two-press owner.
    use_keybinding(
        hooks,
        runtime,
        "chat:killAgents",
        ContextName::Chat,
        || true,
        move || {
            let running = crate::tasks::local_agent_task::running_local_agent_tasks();
            let mut notifications = notifications.clone();
            if running.is_empty() {
                notifications.add_notification(
                    crate::context::notifications::Notification::text(
                        "kill-agents-none",
                        "No background agents running",
                        crate::context::notifications::NotificationPriority::Immediate,
                    )
                    .with_timeout_ms(2_000),
                );
                return true;
            }

            let now = Instant::now();
            if last_kill_agents_press
                .get()
                .is_some_and(|last| now.duration_since(last) <= Duration::from_secs(3))
            {
                last_kill_agents_press.set(None);
                notifications.remove_notification("kill-agents-confirm");
                crate::utils::message_queue_manager::clear_command_queue();
                let killed = crate::tasks::local_agent_task::kill_all_running_agent_tasks();
                let descriptions = killed
                    .iter()
                    .map(|task| format!("\"{}\"", task.description))
                    .collect::<Vec<_>>();
                let summary = if descriptions.len() == 1 {
                    format!(
                        "Background agent {} was stopped by the user.",
                        descriptions[0]
                    )
                } else {
                    format!(
                        "{} background agents were stopped by the user: {}.",
                        descriptions.len(),
                        descriptions.join(", ")
                    )
                };
                // CC `useCancelRequest.ts:192` passes only value + mode —
                // priority falls to enqueuePendingNotification's `?? 'later'`
                // and isMeta stays unset (the stop summary is visible).
                let mut notification = crate::utils::message_queue_manager::QueuedCommand::new(
                    summary,
                    "task-notification",
                );
                notification.priority = crate::utils::message_queue_manager::QueuePriority::Later;
                crate::utils::message_queue_manager::enqueue_pending_notification(notification);
                return true;
            }

            last_kill_agents_press.set(Some(now));
            notifications.add_notification(
                crate::context::notifications::Notification::text(
                    "kill-agents-confirm",
                    format!("Press {kill_agents_shortcut} again to stop background agents"),
                    crate::context::notifications::NotificationPriority::Immediate,
                )
                .with_timeout_ms(3_000),
            );
            true
        },
    );
}

/// The `CancelRequestHandler` props (`useCancelRequest.ts:40-57`) this REPL
/// supplies. CC's `abortSignal` becomes `can_cancel_running_task`; the
/// screen / local-command-UI facts arrive folded into `is_context_blocked`.
#[derive(Default, Props)]
pub struct CancelRequestHandlerProps {
    pub can_cancel_running_task: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    pub is_context_blocked: bool,
}

/// Maps to: CC `useCancelRequest.ts:63-276` `CancelRequestHandler`, the
/// null-rendering component REPL mounts inside `KeybindingSetup`.
#[component]
pub fn CancelRequestHandler(
    props: &CancelRequestHandlerProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let app_store = crate::state::app_state::use_set_app_state(&mut hooks);
    let can_cancel = props
        .can_cancel_running_task
        .clone()
        .expect("CancelRequestHandler can_cancel_running_task");
    let on_cancel = props.on_cancel.clone().expect("CancelRequestHandler on_cancel");
    use_cancel_request(
        &mut hooks,
        UseCancelRequestOptions {
            app_store,
            can_cancel_running_task: move || can_cancel(),
            on_cancel: move || on_cancel(),
            is_context_blocked: props.is_context_blocked,
        },
    );
    element!(View(width: 0u32, height: 0u32))
}
