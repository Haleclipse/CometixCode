//! Leader permission bridge.
//!
//! Maps to: CC `utils/swarm/leaderPermissionBridge.ts` (whole file).
//!
//! CC's header states the job exactly: "Module-level bridge that allows the REPL
//! to register its setToolUseConfirmQueue and setToolPermissionContext functions
//! for in-process teammates to use. [...] This bridge makes the REPL's queue
//! setter and permission context setter accessible from non-React code in the
//! in-process runner."
//!
//! Two module-level slots, six functions, no state of their own — the same
//! shape here. What the port has to translate is the *setter*: CC's
//! `SetToolUseConfirmQueueFn` is a React `setState` updater
//! (`(prev: ToolUseConfirm[]) => ToolUseConfirm[]`), which is not a value
//! setter — it is "read the CURRENT queue and hand back the next one", the only
//! form that is safe from a callback that has been sitting on a pending promise
//! while the queue changed underneath it. Both consumers depend on that:
//! `inProcessRunner.ts:214-216` / `:321-323` withdraw *their own row* by
//! `queue.filter(item => item.toolUseID !== toolUseID)`, and
//! `REPL.tsx:3120-3125` uses an updater that returns `currentQueue` unchanged
//! purely to READ it ("instead of capturing it in the closure, to avoid stale
//! closure issues"). A `Sender<ToolUseConfirm>` cannot express either.
//!
//! So [`ToolUseConfirmQueueUpdater`] is that closure, and
//! [`SetToolUseConfirmQueueFn`] is the registered applier. The REPL's applier
//! ships the updater to the component that owns the `Vec<ToolUseConfirm>` state
//! and applies it there (`screens/repl.rs`), which is also why the port keeps a
//! SINGLE applier for pushes and withdrawals: FIFO on one channel is what makes
//! "queue the row, then withdraw the row" ordered, exactly as CC's one
//! synchronous setter is.

use crate::types::permissions::ToolUseConfirm;
#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// Maps to: CC `leaderPermissionBridge.ts:16-18`
/// `SetToolUseConfirmQueueFn`'s `updater` parameter —
/// `(prev: ToolUseConfirm[]) => ToolUseConfirm[]`.
pub type ToolUseConfirmQueueUpdater =
    Box<dyn FnOnce(Vec<ToolUseConfirm>) -> Vec<ToolUseConfirm> + Send + 'static>;

/// Maps to: CC `leaderPermissionBridge.ts:16-18#SetToolUseConfirmQueueFn`.
#[derive(Clone)]
pub struct SetToolUseConfirmQueueFn(
    #[allow(clippy::type_complexity)] Arc<dyn Fn(ToolUseConfirmQueueUpdater) + Send + Sync>,
);

impl SetToolUseConfirmQueueFn {
    pub fn new<F>(applier: F) -> Self
    where
        F: Fn(ToolUseConfirmQueueUpdater) + Send + Sync + 'static,
    {
        Self(Arc::new(applier))
    }

    /// `setToolUseConfirmQueue(updater)`.
    pub fn call(&self, updater: ToolUseConfirmQueueUpdater) {
        (self.0)(updater)
    }

    /// Maps to: CC `inProcessRunner.ts:214-216` / `:321-323`
    /// `setToolUseConfirmQueue(queue => queue.filter(item =>
    /// item.toolUseID !== toolUseID))` and `interactiveHandler.ts`'s
    /// `ctx.removeFromQueue()` (`PermissionContext.ts:340-342`) — a callback
    /// that has stopped waiting withdraws its own row so the human is not left
    /// answering a prompt nobody is listening to.
    pub fn remove_from_queue(&self, tool_use_id: &str) {
        let tool_use_id = tool_use_id.to_string();
        self.call(Box::new(move |mut queue| {
            crate::hooks::tool_permission::permission_context::remove_from_queue(
                &mut queue,
                &tool_use_id,
            );
            queue
        }));
    }

    /// Maps to: CC `inProcessRunner.ts:210-216` — the abort listener's two
    /// adjacent statements, `decisionMade = true` followed by the same
    /// `queue.filter(...)` withdrawal. A racer that stops waiting must take the
    /// row's claim on its way out, or a dialog answer already in flight would
    /// win a claim nobody is left to honour and persist its rules for a
    /// decision that goes nowhere.
    ///
    /// CC runs both statements synchronously in the listener; here the claim
    /// travels inside the updater because that is the only place with the row
    /// in hand — the responder is built inside the leader sink's per-ask
    /// closure (`interactive_handler.rs#create_repl_interactive_permission_sink`)
    /// and escapes only onto the row that travels this FIFO, so the abort site
    /// holds the `tool_use_id` and nothing else.
    ///
    /// The gap between publishing this updater and the REPL applying it is
    /// therefore real, and it is closed on the ANSWER side rather than here:
    /// `screens/repl.rs#settle_pending_permission_queue_updaters` applies every
    /// published updater before the answer path addresses a row, which makes
    /// this FIFO the port's serialization point and gives an abort published
    /// before a keypress the same precedence CC's synchronous listener has.
    /// That is also why the claim rides the SAME updater as the withdrawal
    /// rather than a second one: settling the FIFO must never apply the
    /// withdrawal without the claim.
    pub fn claim_and_remove_from_queue(&self, tool_use_id: &str) {
        let tool_use_id = tool_use_id.to_string();
        self.call(Box::new(move |mut queue| {
            if let Some(entry) = queue
                .iter()
                .find(|entry| entry.tool_use_id() == tool_use_id)
            {
                entry.responder.claim();
            }
            crate::hooks::tool_permission::permission_context::remove_from_queue(
                &mut queue,
                &tool_use_id,
            );
            queue
        }));
    }

    /// Maps to: CC `interactiveHandler.ts:92` `ctx.pushToQueue({...})` /
    /// `inProcessRunner.ts:223-332` `setToolUseConfirmQueue(queue => [...queue,
    /// {...}])` — the append every producer of a dialog row performs.
    pub fn push_to_queue(&self, confirm: ToolUseConfirm) {
        self.call(Box::new(move |mut queue| {
            crate::hooks::tool_permission::permission_context::push_to_queue(&mut queue, confirm);
            queue
        }));
    }
}

impl std::fmt::Debug for SetToolUseConfirmQueueFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetToolUseConfirmQueueFn(..)")
    }
}

/// Adapter for callers that only want to OBSERVE the rows a producer appends:
/// each updater is applied to an empty queue and whatever it yields is
/// forwarded. Withdrawals therefore find nothing to withdraw — a channel is not
/// a queue, it cannot hold the row a later `queue.filter(...)` would drop — so
/// this is `#[cfg(test)]`: production leaders own a real
/// `Vec<ToolUseConfirm>`, which is what makes CC's updater form meaningful.
#[cfg(test)]
impl From<async_channel::Sender<ToolUseConfirm>> for SetToolUseConfirmQueueFn {
    fn from(sender: async_channel::Sender<ToolUseConfirm>) -> Self {
        Self::new(move |updater| {
            for confirm in updater(Vec::new()) {
                let _ = sender.try_send(confirm);
            }
        })
    }
}

/// Maps to: CC `leaderPermissionBridge.ts:20-23`
/// `SetToolPermissionContextFn`'s `options?: { preserveMode?: boolean }`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetToolPermissionContextOptions {
    pub preserve_mode: bool,
}

impl SetToolPermissionContextOptions {
    /// CC's `undefined` second argument — `options?.preserveMode` is falsy, so
    /// the incoming context's mode is adopted.
    pub fn none() -> Self {
        Self::default()
    }

    /// CC `inProcessRunner.ts:277-279` `{ preserveMode: true }`.
    pub fn preserve_mode() -> Self {
        Self {
            preserve_mode: true,
        }
    }
}

/// Maps to: CC `leaderPermissionBridge.ts:20-23#SetToolPermissionContextFn`.
#[derive(Clone)]
pub struct SetToolPermissionContextFn(
    #[allow(clippy::type_complexity)]
    Arc<dyn Fn(crate::tool::ToolPermissionContext, SetToolPermissionContextOptions) + Send + Sync>,
);

impl SetToolPermissionContextFn {
    pub fn new<F>(setter: F) -> Self
    where
        F: Fn(crate::tool::ToolPermissionContext, SetToolPermissionContextOptions)
            + Send
            + Sync
            + 'static,
    {
        Self(Arc::new(setter))
    }

    pub fn call(
        &self,
        context: crate::tool::ToolPermissionContext,
        options: SetToolPermissionContextOptions,
    ) {
        (self.0)(context, options)
    }
}

impl std::fmt::Debug for SetToolPermissionContextFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetToolPermissionContextFn(..)")
    }
}

/// Maps to: CC `leaderPermissionBridge.ts:25` `let registeredSetter`.
static REGISTERED_SETTER: LazyLock<Mutex<Option<SetToolUseConfirmQueueFn>>> =
    LazyLock::new(|| Mutex::new(None));

/// Maps to: CC `leaderPermissionBridge.ts:26`
/// `let registeredPermissionContextSetter`.
static REGISTERED_PERMISSION_CONTEXT_SETTER: LazyLock<Mutex<Option<SetToolPermissionContextFn>>> =
    LazyLock::new(|| Mutex::new(None));

/// Maps to: CC `leaderPermissionBridge.ts:28-32#registerLeaderToolUseConfirmQueue`.
/// Registered by the REPL on mount (`REPL.tsx:1645-1648`).
pub fn register_leader_tool_use_confirm_queue(setter: SetToolUseConfirmQueueFn) {
    *REGISTERED_SETTER
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(setter);
}

/// Maps to: CC `leaderPermissionBridge.ts:34-36#getLeaderToolUseConfirmQueue`.
pub fn get_leader_tool_use_confirm_queue() -> Option<SetToolUseConfirmQueueFn> {
    REGISTERED_SETTER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Maps to: CC `leaderPermissionBridge.ts:38-40#unregisterLeaderToolUseConfirmQueue`
/// (`REPL.tsx:1647` effect cleanup).
pub fn unregister_leader_tool_use_confirm_queue() {
    *REGISTERED_SETTER
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

/// Maps to: CC
/// `leaderPermissionBridge.ts:42-46#registerLeaderSetToolPermissionContext`
/// (`REPL.tsx:3132-3135`).
pub fn register_leader_set_tool_permission_context(setter: SetToolPermissionContextFn) {
    *REGISTERED_PERMISSION_CONTEXT_SETTER
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(setter);
}

/// Maps to: CC
/// `leaderPermissionBridge.ts:48-50#getLeaderSetToolPermissionContext`
/// (`inProcessRunner.ts:266-267`).
pub fn get_leader_set_tool_permission_context() -> Option<SetToolPermissionContextFn> {
    REGISTERED_PERMISSION_CONTEXT_SETTER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Maps to: CC
/// `leaderPermissionBridge.ts:52-54#unregisterLeaderSetToolPermissionContext`
/// (`REPL.tsx:3134` effect cleanup).
pub fn unregister_leader_set_tool_permission_context() {
    *REGISTERED_PERMISSION_CONTEXT_SETTER
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

/// RAII registration of BOTH halves, held by the REPL while mounted.
///
/// Maps to: CC `REPL.tsx:1645-1648` and `:3131-3135` — two `useEffect`s whose
/// bodies register and whose cleanups unregister. One guard for both because
/// they share a mount boundary and an owner.
///
/// The Rust-only part is Drop: CC's process-lifetime REPL never really
/// unregisters, while an unmounted Cometix REPL must clear the slots so a
/// producer reaching the bridge afterwards finds `None` (CC's null branch:
/// mailbox fallback for a teammate, no sweep for a context write) instead of
/// pushing rows into a queue nothing renders. Precedent:
/// `utils/sandbox/sandbox_adapter.rs#SandboxAskCallbackRegistration`,
/// `state/overlay.rs#OverlayRegistration`.
pub struct LeaderPermissionBridgeRegistration {
    registered_queue: Option<SetToolUseConfirmQueueFn>,
    registered_permission_context: Option<SetToolPermissionContextFn>,
}

impl LeaderPermissionBridgeRegistration {
    pub fn register(
        set_tool_use_confirm_queue: SetToolUseConfirmQueueFn,
        app_store: crate::state::store::AppStore,
    ) -> Self {
        // CC `REPL.tsx:3096-3129` — the leader's `setToolPermissionContext`
        // callback, which is what `:3133` registers.
        let set_tool_permission_context =
            SetToolPermissionContextFn::new(move |context, options| {
                set_leader_tool_permission_context(&app_store, context, options);
            });
        register_leader_tool_use_confirm_queue(set_tool_use_confirm_queue.clone());
        register_leader_set_tool_permission_context(set_tool_permission_context.clone());
        Self {
            registered_queue: Some(set_tool_use_confirm_queue),
            registered_permission_context: Some(set_tool_permission_context),
        }
    }
}

impl Drop for LeaderPermissionBridgeRegistration {
    fn drop(&mut self) {
        if let Some(mine) = self.registered_queue.take() {
            let mut slot = REGISTERED_SETTER
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // Only clear the slot if it is still ours: a later mount already
            // overwrote it, and CC's `registeredSetter = null` cleanup runs
            // before the next effect body in that ordering too.
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.0, &mine.0))
            {
                *slot = None;
            }
        }
        if let Some(mine) = self.registered_permission_context.take() {
            let mut slot = REGISTERED_PERMISSION_CONTEXT_SETTER
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.0, &mine.0))
            {
                *slot = None;
            }
        }
    }
}

/// Maps to: CC `REPL.tsx:3096-3112` — the body of the leader's
/// `setToolPermissionContext` useCallback, the function
/// [`register_leader_set_tool_permission_context`] registers:
///
/// ```ts
/// setAppState(prev => ({
///   ...prev,
///   toolPermissionContext: {
///     ...context,
///     mode: options?.preserveMode ? prev.toolPermissionContext.mode : context.mode,
///   },
/// }))
/// ```
///
/// CC's comment names what `preserveMode` is for: "Workers' getAppState()
/// returns a transformed context with mode 'acceptEdits' that must not leak
/// into the coordinator's actual state via permission-rule updates — those call
/// sites pass `{ preserveMode: true }`. User-initiated mode changes (e.g.,
/// selecting 'allow all edits') must NOT be overridden."
///
/// The sweep half of that callback (`:3114-3126`) is NOT here: this port fires
/// it from `AppStore::set_tool_permission_context` (`state/store.rs`), the
/// single choke point every in-session context write goes through, so a write
/// that did not come through this function still rechecks the queue.
pub fn set_leader_tool_permission_context(
    app_store: &crate::state::store::AppStore,
    context: crate::tool::ToolPermissionContext,
    options: SetToolPermissionContextOptions,
) {
    let mut next = context;
    if options.preserve_mode {
        next.mode = app_store.get().tool_permission_context.mode;
    }
    app_store.set_tool_permission_context(next);
}

/// Maps to: CC `REPL.tsx:3114-3126` and `useReplBridge.tsx:546-554` — the two
/// byte-identical sweeps CC runs after the permission context changes:
///
/// ```ts
/// setImmediate(() => {
///   getLeaderToolUseConfirmQueue()?.(currentQueue => {
///     currentQueue.forEach(item => { void item.recheckPermission() })
///     return currentQueue
///   })
/// })
/// ```
///
/// CC's own reason (`:3114-3116`): "When permission context changes, recheck all
/// queued items. This handles the case where approving item1 with 'don't ask
/// again' should auto-approve other queued items that now match the updated
/// rules."
///
/// Three properties of CC's shape are load-bearing and kept:
/// - the updater returns `currentQueue` UNCHANGED — the sweep only reads; each
///   entry that resolves withdraws itself (`:321-323`);
/// - it runs through the registered setter rather than a captured snapshot, so
///   it observes the queue as of now ("to avoid stale closure issues");
/// - `setImmediate` + `void` — the recheck is detached, so the write that
///   triggered it returns immediately. Here: `runtime_handle_for_detached_work`
///   (this port's name for CC's one event loop), which yields `None` for
///   callers with no process runtime published — those simply do not sweep,
///   the same no-op CC gets from `getLeaderToolUseConfirmQueue()?.` being null
///   outside the REPL.
pub fn recheck_queued_permissions(app_store: &crate::state::store::AppStore) {
    let Some(setter) = get_leader_tool_use_confirm_queue() else {
        return;
    };
    let Some(runtime) = crate::utils::process_runtime::runtime_handle_for_detached_work() else {
        return;
    };
    let app_store = app_store.clone();
    setter.call(Box::new(move |queue| {
        for item in &queue {
            let entry = item.clone();
            let app_store = app_store.clone();
            let setter = get_leader_tool_use_confirm_queue();
            // CC `void item.recheckPermission()`.
            runtime.spawn(async move {
                crate::hooks::tool_permission::handlers::interactive_handler::recheck_permission(
                    &entry, &app_store, setter,
                )
                .await;
            });
        }
        queue
    }));
}

#[cfg(test)]
pub static TEST_LEADER_PERMISSION_BRIDGE_LOCK: LazyLock<TestStateLock> =
    LazyLock::new(TestStateLock::new);

/// A queue owner for tests: the `Vec<ToolUseConfirm>` the REPL keeps in
/// `useState` (`REPL.tsx:1529`), the applier that reaches it, and a stream of
/// every version an applier produced (what a re-render would observe).
#[cfg(test)]
pub struct TestLeaderQueue {
    pub setter: SetToolUseConfirmQueueFn,
    pub queue: Arc<Mutex<Vec<ToolUseConfirm>>>,
    pub versions: async_channel::Receiver<Vec<ToolUseConfirm>>,
}

#[cfg(test)]
pub fn test_leader_queue() -> TestLeaderQueue {
    let queue: Arc<Mutex<Vec<ToolUseConfirm>>> = Arc::new(Mutex::new(Vec::new()));
    let (versions_tx, versions) = async_channel::unbounded();
    let queue_for_setter = queue.clone();
    let setter = SetToolUseConfirmQueueFn::new(move |updater| {
        let mut guard = queue_for_setter
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let previous = std::mem::take(&mut *guard);
        *guard = updater(previous);
        let _ = versions_tx.try_send(guard.clone());
    });
    TestLeaderQueue {
        setter,
        queue,
        versions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionMode, PermissionRequest, PermissionRuleValue};

    /// `cargo build`, not `echo`: `echo` is semantically neutral
    /// (`bash_tool/mod.rs#BASH_SEMANTIC_NEUTRAL_COMMANDS`) and allows outright,
    /// which would make a "still asking" assertion vacuous.
    fn request(tool_use_id: &str) -> PermissionRequest {
        PermissionRequest {
            permission_result: None,
            id: format!("perm-{tool_use_id}"),
            tool_use_id: tool_use_id.to_string(),
            tool_name: "Bash".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "Run command?".to_string(),
            message: String::new(),
            input_summary: "cargo build".to_string(),
            input: serde_json::json!({ "command": "cargo build" }),
            call_input: None,
            rule: PermissionRuleValue::new("Bash", Some("cargo build".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    /// Maps to: CC `leaderPermissionBridge.ts:25-40` — one module-level slot,
    /// `get` returns whatever `register` last stored and `null` after
    /// `unregister` (`REPL.tsx:1646-1647` mount/unmount).
    #[test]
    fn queue_registration_round_trips_and_unregisters() {
        let _bridge_lock = TEST_LEADER_PERMISSION_BRIDGE_LOCK.lock().unwrap();
        unregister_leader_tool_use_confirm_queue();
        assert!(get_leader_tool_use_confirm_queue().is_none());

        let leader = test_leader_queue();
        register_leader_tool_use_confirm_queue(leader.setter.clone());
        get_leader_tool_use_confirm_queue()
            .expect("registered")
            .push_to_queue(ToolUseConfirm::new(request("toolu_1")));
        assert_eq!(leader.queue.lock().unwrap().len(), 1);

        unregister_leader_tool_use_confirm_queue();
        assert!(get_leader_tool_use_confirm_queue().is_none());
    }

    /// Maps to: CC `inProcessRunner.ts:214-216` — the updater form is what
    /// makes a withdrawal possible at all: the caller does not hold the queue,
    /// it hands back `queue.filter(...)`.
    #[test]
    fn remove_from_queue_withdraws_only_the_named_row() {
        let leader = test_leader_queue();
        leader
            .setter
            .push_to_queue(ToolUseConfirm::new(request("toolu_1")));
        leader
            .setter
            .push_to_queue(ToolUseConfirm::new(request("toolu_2")));

        leader.setter.remove_from_queue("toolu_1");

        let remaining = leader.queue.lock().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].tool_use_id(), "toolu_2");
    }

    /// Maps to: CC `inProcessRunner.ts:210-216` — the abort listener sets
    /// `decisionMade = true` and then filters the row out. A dialog answer
    /// already in flight must find the claim gone, so it stops before
    /// `persistPermissionUpdates` (`:263`).
    ///
    /// OLD SHAPE: the abort arm called plain `remove_from_queue`, leaving the
    /// row unclaimed. A user answering in that window took the claim, wrote the
    /// rule to disk and moved the live context for a decision whose waiter had
    /// already given up.
    #[test]
    fn claim_and_remove_takes_the_rows_claim_before_withdrawing_it() {
        let leader = test_leader_queue();
        let (response_tx, _response_rx) = async_channel::bounded(1);
        let aborted = ToolUseConfirm::new(request("toolu_aborted")).with_responder(
            crate::types::permissions::PermissionPromptResponder::new(response_tx),
        );
        let (other_tx, _other_rx) = async_channel::bounded(1);
        let untouched = ToolUseConfirm::new(request("toolu_other")).with_responder(
            crate::types::permissions::PermissionPromptResponder::new(other_tx),
        );
        leader.setter.push_to_queue(aborted.clone());
        leader.setter.push_to_queue(untouched.clone());

        leader.setter.claim_and_remove_from_queue("toolu_aborted");

        let remaining = leader.queue.lock().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].tool_use_id(), "toolu_other");
        assert!(
            !aborted.responder.claim(),
            "the withdrawn row's claim is taken, so a late answer loses it"
        );
        assert!(untouched.responder.claim(), "only the named row is claimed");
    }

    /// Maps to: CC `REPL.tsx:3114-3126` — the sweep that runs after the
    /// permission context changes, resolving a queued row whose fresh
    /// `hasPermissionsToUseTool` now says `allow`
    /// (`inProcessRunner.ts:305-330` body).
    ///
    /// OLD SHAPE: `AppStore::set_tool_permission_context` was a bare
    /// `replace_with`, the bridge did not exist, and `ToolUseConfirm` had no
    /// recheck at all — so the granted rule reached the store and the queued
    /// row just sat there. That failure is a HANG (the asking query waits on a
    /// dialog that no longer needs answering), so the assertion is behind a
    /// timeout rather than a bare `await`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_context_change_that_turns_a_queued_ask_into_an_allow_resolves_it() {
        let _bridge_lock = TEST_LEADER_PERMISSION_BRIDGE_LOCK.lock().unwrap();
        unregister_leader_tool_use_confirm_queue();

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let leader = test_leader_queue();
        register_leader_tool_use_confirm_queue(leader.setter.clone());

        // The row a subagent/teammate is parked on: it carries its own
        // resolver, exactly as CC's queued entry closes over `resolve`.
        let (response_tx, response_rx) = async_channel::bounded(1);
        leader
            .setter
            .push_to_queue(ToolUseConfirm::new(request("toolu_sweep")).with_responder(
                crate::types::permissions::PermissionPromptResponder::new(response_tx),
            ));

        // A context write that grants nothing sweeps too, and must leave the
        // row parked — otherwise the assertion below would pass on a request
        // that was never really an ask.
        store.set_tool_permission_context(store.tool_permission_context());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(response_rx.try_recv().is_err());
        assert_eq!(leader.queue.lock().unwrap().len(), 1);

        // The context change: a whole-tool session allow for Bash, i.e. what
        // answering some OTHER prompt with "don't ask again" leaves behind.
        let mut granted = store.tool_permission_context();
        granted.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        store.set_tool_permission_context(granted);

        let response = tokio::time::timeout(std::time::Duration::from_secs(5), response_rx.recv())
            .await
            .expect("the sweep must resolve the queued ask, not leave it parked")
            .expect("responder must deliver");
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::AllowOnce
        );

        // CC `:321-323` — a row that resolved itself leaves the dialog queue.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !leader.queue.lock().unwrap().is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the resolved row must be withdrawn from the leader's queue"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        unregister_leader_tool_use_confirm_queue();
    }

    /// The other half of the sweep's gate: with no registered leader queue
    /// (headless, tests, print) the write is CC's `getLeaderToolUseConfirmQueue()?.`
    /// null branch — a plain context replace and nothing else.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_context_write_without_a_registered_leader_queue_is_a_plain_replace() {
        let _bridge_lock = TEST_LEADER_PERMISSION_BRIDGE_LOCK.lock().unwrap();
        unregister_leader_tool_use_confirm_queue();

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut granted = store.tool_permission_context();
        granted.mode = PermissionMode::AcceptEdits;
        store.set_tool_permission_context(granted);

        assert_eq!(
            store.tool_permission_context().mode,
            PermissionMode::AcceptEdits
        );
    }

    /// Maps to: CC `REPL.tsx:3108-3110`
    /// `mode: options?.preserveMode ? prev.toolPermissionContext.mode : context.mode`.
    /// Everything except `mode` comes from the incoming context in both arms —
    /// CC spreads `...context` first and only overrides `mode`.
    #[test]
    fn preserve_mode_keeps_the_leaders_mode_and_takes_every_other_field() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut leader = store.tool_permission_context();
        leader.mode = PermissionMode::Plan;
        store.set_tool_permission_context(leader);

        // What a worker's transformed context looks like coming back:
        // acceptEdits mode plus the rule the user just granted.
        let mut worker = store.tool_permission_context();
        worker.mode = PermissionMode::AcceptEdits;
        worker.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("echo hi".to_string()),
            )],
        );

        set_leader_tool_permission_context(
            &store,
            worker.clone(),
            SetToolPermissionContextOptions::preserve_mode(),
        );
        let after_preserve = store.tool_permission_context();
        assert_eq!(after_preserve.mode, PermissionMode::Plan);
        assert_eq!(
            after_preserve.always_allow_rules, worker.always_allow_rules,
            "preserveMode guards the mode only; the rules still land"
        );

        set_leader_tool_permission_context(&store, worker, SetToolPermissionContextOptions::none());
        assert_eq!(
            store.tool_permission_context().mode,
            PermissionMode::AcceptEdits,
            "without the option CC adopts the incoming mode (user-initiated changes)"
        );
    }

    /// Maps to: CC `leaderPermissionBridge.ts:42-54` — the permission-context
    /// half registers/unregisters on the same mount boundary
    /// (`REPL.tsx:3132-3135`).
    #[test]
    fn permission_context_registration_round_trips_and_unregisters() {
        let _bridge_lock = TEST_LEADER_PERMISSION_BRIDGE_LOCK.lock().unwrap();
        unregister_leader_set_tool_permission_context();
        assert!(get_leader_set_tool_permission_context().is_none());

        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_setter = seen.clone();
        register_leader_set_tool_permission_context(SetToolPermissionContextFn::new(
            move |context, options| {
                seen_for_setter
                    .lock()
                    .unwrap()
                    .push((context.mode, options.preserve_mode));
            },
        ));
        get_leader_set_tool_permission_context()
            .expect("registered")
            .call(
                crate::tool::ToolPermissionContext::default(),
                SetToolPermissionContextOptions::preserve_mode(),
            );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(crate::tool::ToolPermissionContext::default().mode, true)]
        );

        unregister_leader_set_tool_permission_context();
        assert!(get_leader_set_tool_permission_context().is_none());
    }
}
