//! Maps to: CC `state/store.ts` — the ~35-line framework-agnostic external
//! store that backs AppState.
//!
//! Deliberately independent of iocraft: CC shares the exact same store type
//! between the interactive React tree (`components/App.tsx`) and the headless
//! SDK path (`main.tsx:3728`). nyan preserves that property so a future
//! print mode (`-p`) reuses this store without a render loop. The TUI is wired
//! in via `state::app_state`.
//!
//! Semantics mirrored 1:1 from `store.ts#createStore`:
//! - `set_state` runs the updater against the latest root; `Same` = CC's
//!   updater returning `prev` (`Object.is` short-circuit, `store.ts:23`),
//!   `Replace` installs the fresh root then runs `on_change` BEFORE
//!   notifying listeners. A value-equal NEW root installs and notifies —
//!   the CC `{...prev}` spread semantics (B3 flip complete 2026-08-02).
//! - Listeners receive no arguments — change granularity is the consumer's
//!   concern (CC: `useSyncExternalStore` selector; Rust: full re-render or
//!   memo pruning).
//!
//! P3 B2 (Contract A clauses 2-6): every mutation runs inside the
//! process-wide re-entrant [`StoreTurn`] (CC one event-loop turn); the
//! updater and all effect callbacks run with NO lock held; the listener
//! registry is a live insertion-order `Arc`-identity set (CC `Set`,
//! `store.ts:15,26,29-32`) with stateless-identity [`Unsubscribe`] handles
//! and generation-cursor live iteration; panic boundaries per clause 5 are
//! structural (install-before-effects + unwind + RAII turn release, no
//! poisoning).
//!
//! The only equality anywhere in this store is root identity
//! (`Arc::ptr_eq` ≙ `Object.is`); no field-level or value-level comparison
//! exists (Contract A clause 1).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, Weak};

use super::app_state_store::AppState;

/// Maps to: CC `store.ts` state root — the `T` (= AppState) held behind the
/// module closure. Shared immutable root; a new root per effective update.
pub type AppStateRoot = Arc<AppState>;

// ---------------------------------------------------------------------------
// Process-wide re-entrant store turn (Contract A clauses 4/6)
// ---------------------------------------------------------------------------

/// Rust-only, no CC counterpart, `L1 (Cross-thread synchronous AppStore turn
/// carrier)`: CC one JavaScript event-loop turn ≙ one process-wide re-entrant
/// Rust turn covering the store family (AppStore mutations + effect passes,
/// SandboxViolationStore public-operation segments). Same-thread nesting is
/// legal and runs synchronously (CC synchronous recursion); other threads
/// block until the turn is released. The `ToolUseContext` domain keeps its
/// own `FileReadSourceTurn` gate — holding that gate may enter this turn,
/// never the reverse (clause 6 ordering, debug-asserted at the FileRead
/// gate).
struct StoreTurn {
    owner: Mutex<TurnOwner>,
    released: Condvar,
}

#[derive(Default)]
struct TurnOwner {
    holder: Option<std::thread::ThreadId>,
    depth: u32,
}

/// RAII turn segment; dropping releases one nesting level (panic-safe: an
/// unwinding effect pass releases the turn without poisoning it, clause 5).
///
/// `!Send` (raw-pointer marker): clause 4 forbids awaiting inside the turn —
/// a future holding this guard across an `.await` loses `Send`, so a
/// multi-threaded runtime rejects it at compile time (the structural
/// strengthening of the clause's "debug assert"). Joining a worker that
/// needs this turn cannot be asserted statically; it surfaces as the
/// Condvar deadlock the clause warns about.
struct StoreTurnGuard {
    turn: &'static StoreTurn,
    _not_send: std::marker::PhantomData<*mut ()>,
}

impl StoreTurn {
    #[must_use]
    fn enter(&'static self) -> StoreTurnGuard {
        let me = std::thread::current().id();
        let mut owner = self.owner.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match owner.holder {
                None => {
                    owner.holder = Some(me);
                    owner.depth = 1;
                    break;
                }
                Some(holder) if holder == me => {
                    owner.depth += 1;
                    break;
                }
                Some(_) => {
                    owner = self
                        .released
                        .wait(owner)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
        StoreTurnGuard {
            turn: self,
            _not_send: std::marker::PhantomData,
        }
    }

    fn held_by_current_thread(&self) -> bool {
        let owner = self.owner.lock().unwrap_or_else(PoisonError::into_inner);
        owner.holder == Some(std::thread::current().id())
    }
}

impl Drop for StoreTurnGuard {
    fn drop(&mut self) {
        let mut owner = self
            .turn
            .owner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        owner.depth -= 1;
        if owner.depth == 0 {
            owner.holder = None;
            drop(owner);
            self.turn.released.notify_one();
        }
    }
}

fn store_turn() -> &'static StoreTurn {
    static TURN: std::sync::OnceLock<StoreTurn> = std::sync::OnceLock::new();
    TURN.get_or_init(|| StoreTurn {
        owner: Mutex::new(TurnOwner::default()),
        released: Condvar::new(),
    })
}

/// Enter the process-wide store turn for one synchronous public operation of
/// a non-`store.ts` turn member (Contract A clause 6: e.g.
/// `SandboxViolationStore` segments). Same-thread re-entry nests.
///
/// Rust-only, no CC counterpart, `L1 (Cross-thread synchronous AppStore turn
/// carrier)` — the segment entry point for turn members outside this module.
#[must_use = "dropping the guard immediately ends the turn segment"]
pub fn enter_store_turn_segment() -> impl Drop {
    store_turn().enter()
}

/// Whether the current thread holds the store turn. Used by the FileRead
/// gate's clause-6 ordering debug assert.
///
/// Rust-only, no CC counterpart, `L1 (Cross-thread synchronous AppStore turn
/// carrier)` — ordering introspection for the clause-6 acquisition rule.
pub fn store_turn_held_by_current_thread() -> bool {
    store_turn().held_by_current_thread()
}

/// Updater outcome for [`AppStore::set_state`].
///
/// Rust-only, no CC counterpart, `L1 (Cross-thread synchronous AppStore turn
/// carrier)`. CC expresses the same two outcomes structurally: `Same` = the
/// updater returning `prev` (caught by `store.ts:23` `Object.is(next, prev)`),
/// `Replace` = returning a fresh object (`{...prev, ...}`).
pub enum UpdateDecision<R> {
    /// Keep the current root: no install, no `on_change`, no listener pass.
    Same(R),
    /// Install `next` as the new root, then run `on_change` → listeners.
    Replace { next: AppStateRoot, result: R },
}

/// Maps to: CC `store.ts` `OnChange<T>` — `({newState, oldState}) => void`.
pub type OnChangeFn = Arc<dyn Fn(&AppState, &AppState) + Send + Sync>;

/// Maps to: CC `store.ts` `Listener` — `() => void`. Identity is the `Arc`
/// pointer (CC: the function object identity keyed by the `Set`).
pub type Listener = Arc<dyn Fn() + Send + Sync>;

/// One registry entry. `generation` is assigned at insertion and is used ONLY
/// as the live-iteration cursor (Contract A clause 3: it must not participate
/// in removal, which is pure `Arc::ptr_eq` identity). A remove + re-add of
/// the same `Arc` gets a fresh generation, which is what makes the re-added
/// entry visible again at the tail of an in-flight pass (JS `Set` delete +
/// add moves the entry to the end and the iterator revisits it).
struct ListenerEntry {
    generation: u64,
    listener: Listener,
}

/// Maps to: CC `store.ts:31` — the returned unsubscribe closure
/// `() => listeners.delete(listener)`. Rust-only NAMED carrier for that
/// closure under `L1 (Cross-thread synchronous AppStore turn carrier)`.
/// Stateless identity delete: every call removes the callback's CURRENT
/// membership by `Arc::ptr_eq` (idempotent no-op when absent), regardless of
/// which subscribe call produced the handle and of any re-subscription in
/// between (Contract A clause 3, 2026-08-02 revision: no registration
/// id/generation binding, no consumed flag).
#[derive(Clone)]
pub struct Unsubscribe {
    inner: Weak<AppStoreInner>,
    listener: Listener,
}

impl Unsubscribe {
    pub fn unsubscribe(&self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let _turn = store_turn().enter();
        inner
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|entry| !Arc::ptr_eq(&entry.listener, &self.listener));
    }
}

impl std::fmt::Debug for Unsubscribe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unsubscribe(..)")
    }
}

struct AppStoreInner {
    state: RwLock<Arc<AppState>>,
    /// Insertion-ordered live registry (CC `Set`, `store.ts:15`). The lock is
    /// never held across a callback (clause 2): the notify pass re-locks per
    /// step (live index-with-recheck over generations).
    listeners: Mutex<Vec<ListenerEntry>>,
    next_generation: AtomicU64,
    /// Late-bindable so the interactive path can construct the store before
    /// the pipeline exists (Contract B clause 4: CC has no store/onChange
    /// pipeline before `AppStateProvider` mounts). Headless binds at
    /// construction, matching CC `main.tsx:3728-3731`.
    on_change: RwLock<Option<OnChangeFn>>,
    /// Bumped only on an effective root install (Contract A: policy-free).
    revision: AtomicU64,
}

/// Maps to: CC `store.ts#createStore` return value. Cheap-to-clone handle;
/// `Send + Sync` so tool-execution tasks on other tokio threads can read and
/// write the store directly (CC passes getState/setState into non-React code
/// via `useAppStateStore`, e.g. `getToolUseContext`).
#[derive(Clone)]
pub struct AppStore {
    inner: Arc<AppStoreInner>,
}

impl std::fmt::Debug for AppStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppStore(..)")
    }
}

impl AppStore {
    /// Native identity carrier for the stable JS store/setState dependency.
    /// Maps to: CC `state/store.ts#createStore` retained return object.
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Maps to: CC `createStore(initialState, onChange)`.
    pub fn new(initial_state: AppState, on_change: Option<OnChangeFn>) -> Self {
        Self {
            inner: Arc::new(AppStoreInner {
                state: RwLock::new(Arc::new(initial_state)),
                listeners: Mutex::new(Vec::new()),
                next_generation: AtomicU64::new(0),
                on_change: RwLock::new(on_change),
                revision: AtomicU64::new(0),
            }),
        }
    }

    /// Attach the change pipeline at Provider adoption.
    ///
    /// Maps to: CC `AppState.tsx:73-78` — `createStore(initialState,
    /// onChangeAppState)` runs inside the Provider's `useState` initializer,
    /// so the pipeline only exists from mount onward. Rust-only named
    /// carrier under `L1 (Prebuilt interactive AppStore provider adoption)`:
    /// the interactive store is prebuilt during the retained launch phase,
    /// so its handler is bound later instead of at construction. Writes made
    /// before this call are silent (Contract B clause 4), matching CC's
    /// pre-mount `initialState` construction.
    ///
    /// Headless keeps construction-time binding ([`AppStore::new`]), matching
    /// CC `main.tsx:3728-3731`.
    ///
    /// Runs inside the process-wide turn (Contract A clause 4), which is what
    /// makes adoption a **linearization point**: `set_state` reads the handler
    /// after installing the root, so without the turn a pre-mount updater
    /// already inside `set_state` could have the Provider bind underneath it
    /// and then fire `on_change` for a write Contract B clause 4 requires to
    /// be silent. Entering the turn orders the bind either wholly before or
    /// wholly after any in-flight mutation.
    pub fn bind_on_change(&self, on_change: OnChangeFn) {
        let _turn = store_turn().enter();
        let mut slot = self
            .inner
            .on_change
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            slot.is_none(),
            "AppStore change pipeline bound twice (CC binds exactly once, at mount)"
        );
        *slot = Some(on_change);
    }

    /// Maps to: CC `store.getState()`. Returns an immutable snapshot; the
    /// `Arc` clone is O(1).
    pub fn get(&self) -> Arc<AppState> {
        self.inner
            .state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Owned snapshot of the current permission context.
    ///
    /// KNOWN DEVIATION (#39, 2026-08-09) — this and `set_tool_permission_context`
    /// below are the only two `AppStore` entries with no CC counterpart and no
    /// L1 contract behind them. CC's `Store<T>` (`state/store.ts:4-8`) is
    /// generic and therefore *cannot* know about any field: its ignorance is a
    /// type-level rule that forces every access through `getState`/`setState`.
    /// Writing `AppState` into `AppStore` removed that rule, and these two are
    /// what grew in the gap. Both are pure forwarding — `get()` and
    /// `replace_with` express them exactly — so they buy convenience, not
    /// capability.
    ///
    /// Kept rather than removed: ~15 call sites across repl/query_engine/tool
    /// already route through them, and churning those to uphold a rule the
    /// codebase does not otherwise enforce is not worth the diff today.
    ///
    /// **The rule this records: do not add a third.** Any new per-field
    /// accessor on `AppStore` must go through `get()` / `set_state` /
    /// `replace_with` instead.
    ///
    /// **Known obstacle**: CC instantiates `createStore` with four different
    /// `T` (`AppState`, `VoiceState` at `context/voice.tsx:36`, `boolean` at
    /// `services/compact/compactWarningState.ts:8`, and the headless store at
    /// `main.tsx:3728`). Only the `AppState` one is ported, which is why the
    /// concrete `AppStore` is currently correct rather than a deviation. When
    /// the second `T` lands, `AppStore` must be generified rather than copied —
    /// and these two accessors are exactly what has to be deleted first,
    /// because a generic store cannot carry `AppState`-specific methods.
    pub fn tool_permission_context(&self) -> crate::tool::ToolPermissionContext {
        (*self.get().tool_permission_context).clone()
    }

    /// Replace permission context (flows through `on_change_app_state`).
    ///
    /// KNOWN DEVIATION — see `tool_permission_context` above.
    ///
    /// The tail is CC `REPL.tsx:3114-3126`: the leader's
    /// `setToolPermissionContext` follows its `setAppState` with a
    /// `setImmediate` sweep that rechecks every queued `ToolUseConfirm`,
    /// "handl[ing] the case where approving item1 with 'don't ask again' should
    /// auto-approve other queued items that now match the updated rules"
    /// (`:3114-3116`), and `useReplBridge.tsx:546-554` repeats it verbatim
    /// after a bridge-driven mode change. CC attaches it to the REPL callback
    /// because in CC that callback IS the write path (`useCanUseTool` and every
    /// `PermissionContext` consumer receive it as an argument). This port has
    /// no such argument — every in-session write lands here — so the sweep
    /// hangs off the choke point instead, which also covers writers CC reaches
    /// through the same callback.
    ///
    /// It is detached and null-guarded on both sides (no registered leader
    /// queue, or no process runtime to detach onto → no-op), so headless and
    /// test callers behave exactly as CC's `getLeaderToolUseConfirmQueue()?.`
    /// does outside the REPL.
    pub fn set_tool_permission_context(&self, context: crate::tool::ToolPermissionContext) {
        self.replace_with(|state| state.set_tool_permission_context(context));
        crate::utils::swarm::leader_permission_bridge::recheck_queued_permissions(self);
    }

    /// Maps to: CC `store.setState(updater)` — the root-identity form
    /// (Contract A `Cross-thread synchronous AppStore turn carrier`).
    ///
    /// The updater observes the current root and either keeps it
    /// ([`UpdateDecision::Same`] = CC's updater returning `prev`) or supplies
    /// a fresh root ([`UpdateDecision::Replace`] = `{...prev, ...}`). `Same`
    /// returns without any effect pass. On `Replace` the new root is
    /// installed (revision bump), then CC ordering applies: state visible →
    /// `on_change(new, old)` → listeners.
    /// B2: the whole call is one turn segment. The updater and every effect
    /// callback run WITHOUT any lock held (clause 2: the state lock never
    /// spans the updater, the registry lock never spans a callback) — mutual
    /// exclusion comes from the process-wide re-entrant turn, so a listener's
    /// nested `set_state` recurses synchronously on the same thread (clause
    /// 4) while other threads block.
    ///
    /// Survey note (2026-08-09), so the next reader does not have to re-derive
    /// it: the RE-ENTRANT half of that turn currently has NO production user.
    /// - CC cannot nest at all — `onChangeAppState` takes `{newState, oldState}`
    ///   and imports only `type AppState`, so it holds no store handle; and
    ///   `store.subscribe` has exactly one consumer, `useSyncExternalStore`,
    ///   whose listener only schedules a re-render.
    /// - Rust's only production listener is `app_state.rs:430`
    ///   `subscribe(Arc::new(move || wake.wake()))` — a one-line body.
    /// - The cross-store case the clause was written for (a
    ///   `SandboxViolationStore` listener re-entering `AppStore`) exists solely
    ///   in `sandbox_adapter.rs`'s clause-6 evidence test; that store has zero
    ///   production subscribers.
    ///
    /// Cross-thread mutual exclusion (the other half) IS load-bearing —
    /// concurrent `set_state` from separate tasks is real. Re-entrancy is
    /// currently reserve capacity for a CC semantic that CC itself does not
    /// exercise. Keep it, but do not cite "we must support nested writes" as
    /// evidence for anything without re-running the survey above. Panic semantics (clause 5) hold
    /// structurally: an updater panic unwinds before the install (no root
    /// change), an `on_change`/listener panic unwinds after it (new root
    /// kept, rest of the pass skipped), the turn guard releases on unwind,
    /// and no lock is poisoned because no user code runs under one.
    pub fn set_state<R>(&self, updater: impl FnOnce(&AppStateRoot) -> UpdateDecision<R>) -> R {
        let _turn = store_turn().enter();
        let prev = self
            .inner
            .state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match updater(&prev) {
            UpdateDecision::Same(result) => result,
            UpdateDecision::Replace { next, result } => {
                // Contract A clause 1: defensive root-identity suppress —
                // an updater handing back the same root is a `Same`. This is
                // the ONLY equality in the store (CC `Object.is`); a
                // value-equal NEW root installs and notifies (CC `{...prev}`
                // spread semantics; B3 flip 2026-08-02 removed the staging
                // value-equality suppression).
                if Arc::ptr_eq(&next, &prev) {
                    return result;
                }
                {
                    let mut guard = self
                        .inner
                        .state
                        .write()
                        .unwrap_or_else(PoisonError::into_inner);
                    *guard = next.clone();
                    // Bumped inside the install critical section so bump and
                    // install are mutually ordered. A standalone `revision()`
                    // load is Relaxed (no happens-before pairing with the
                    // root by itself); Contract C's frame carrier must use
                    // register-then-recheck atomic pairs, not this counter
                    // alone (revisit at P5/P6).
                    self.inner.revision.fetch_add(1, Ordering::Relaxed);
                }
                // CC ordering: state already visible → onChange → listeners.
                // Clause 2: clone the handler out and release the lock before
                // calling it — no lock is ever held across a callback.
                let on_change = self
                    .inner
                    .on_change
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                if let Some(on_change) = on_change {
                    on_change(&next, &prev);
                }
                self.notify_listeners();
                result
            }
        }
    }

    /// Maps to: CC `{...prev}` spread updaters — clone the current root,
    /// mutate the copy, always `Replace`. For call sites whose CC
    /// counterpart unconditionally returns a fresh object.
    ///
    /// Rust-only convenience over [`AppStore::set_state`] — no CC counterpart
    /// symbol; semantically = CC `{...prev, ...}` spread. Post-flip (B3,
    /// 2026-08-02) it notifies unconditionally — exactly the CC spread
    /// semantics Contract A clause 1 mandates (KEEP disposition recorded in
    /// the Contract A amendment; it is not a value-equality compat entry).
    pub fn replace_with<R>(&self, mutator: impl FnOnce(&mut AppState) -> R) -> R {
        self.set_state(|prev| {
            let mut next = (**prev).clone();
            let result = mutator(&mut next);
            UpdateDecision::Replace {
                next: Arc::new(next),
                result,
            }
        })
    }

    /// Monotonic count of effective root installs (Contract A: increments
    /// only on effective replace; policy-free). Every install path bumps it
    /// (the legacy `update` bypass was deleted at the B3 flip).
    ///
    /// A standalone load is `Relaxed` and carries no happens-before pairing
    /// with the root — use [`AppStore::snapshot`] whenever the two must agree.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Relaxed)
    }

    /// Read the root and its revision as ONE consistent pair.
    ///
    /// Rust-only, no CC counterpart, `L1 (iocraft coherent AppState frame
    /// carrier)`. `set_state` bumps the revision inside the same critical
    /// section that installs the root (`store.rs` install path), so taking the
    /// read lock here excludes any in-flight install and the two values cannot
    /// straddle one. Reading `get()` and `revision()` separately can:
    /// an install landing between them yields the old root with the new count,
    /// which would make a frame claim to be newer than it is.
    ///
    /// Contract C clause 3 requires exactly this pairing for the frame
    /// carrier's register-then-recheck; the note left at the B1 review
    /// ("revisit at P5/P6") is discharged here.
    pub fn snapshot(&self) -> (Arc<AppState>, u64) {
        let guard = self
            .inner
            .state
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let revision = self.inner.revision.load(Ordering::Acquire);
        (guard.clone(), revision)
    }

    /// Maps to: CC `store.subscribe(listener)` (`store.ts:29-32`). The `Set`
    /// add coalesces a duplicate of the same `Arc` identity into the single
    /// existing entry; the returned [`Unsubscribe`] is the stateless identity
    /// delete closure (Contract A clause 3).
    pub fn subscribe(&self, listener: Listener) -> Unsubscribe {
        let _turn = store_turn().enter();
        {
            let mut entries = self
                .inner
                .listeners
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !entries
                .iter()
                .any(|entry| Arc::ptr_eq(&entry.listener, &listener))
            {
                let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
                entries.push(ListenerEntry {
                    generation,
                    listener: Arc::clone(&listener),
                });
            }
        }
        Unsubscribe {
            inner: Arc::downgrade(&self.inner),
            listener,
        }
    }

    /// Live insertion-order pass (Contract A clause 3, CC `Set` iteration,
    /// `store.ts:26`): each step re-locks the registry and picks the first
    /// entry whose generation has not been visited this pass, then calls it
    /// with no lock held. This reproduces JS `Set` iterator semantics —
    /// mid-pass removals are skipped, appends run at the tail of the same
    /// pass, and a remove + re-add (fresh generation) is revisited at the
    /// tail. Nested passes (a listener's own `set_state`) mutate the same
    /// registry, and the outer cursor observes those mutations on its next
    /// re-lock.
    fn notify_listeners(&self) {
        let mut visited: Vec<u64> = Vec::new();
        loop {
            let next = {
                let entries = self
                    .inner
                    .listeners
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                entries
                    .iter()
                    .find(|entry| !visited.contains(&entry.generation))
                    .map(|entry| (entry.generation, Arc::clone(&entry.listener)))
            };
            let Some((generation, listener)) = next else {
                break;
            };
            visited.push(generation);
            listener();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::ExpandedView;
    use std::sync::atomic::AtomicUsize;

    fn counting_listener(counter: &Arc<AtomicUsize>) -> Listener {
        let counter = counter.clone();
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
    }

    /// B3 flip: CC `{...prev}` always yields a fresh object, so `Object.is`
    /// never short-circuits a spread — a value-equal NEW root notifies. Only
    /// `Same` (the updater returning `prev`) skips.
    #[test]
    fn replace_with_notifies_on_value_equal_new_root_like_cc_spread() {
        let notified = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);
        store.subscribe(counting_listener(&notified));

        store.replace_with(|_state| {}); // no-op mutation → fresh root
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert_eq!(store.revision(), 1);

        store.replace_with(|state| state.verbose = true);
        assert_eq!(notified.load(Ordering::SeqCst), 2);

        store.replace_with(|state| state.verbose = true); // same value again
        assert_eq!(notified.load(Ordering::SeqCst), 3);
        assert_eq!(store.revision(), 3);

        let result = store.set_state(|_prev| UpdateDecision::Same("skipped"));
        assert_eq!(result, "skipped");
        assert_eq!(notified.load(Ordering::SeqCst), 3, "Same never notifies");
    }

    #[test]
    fn on_change_runs_before_listeners_with_old_and_new_state() {
        let order = Arc::new(RwLock::new(Vec::<String>::new()));
        let order_for_change = order.clone();
        let order_for_listener = order.clone();

        let store = AppStore::new(
            AppState::default(),
            Some(Arc::new(move |new, old| {
                assert!(!old.verbose);
                assert!(new.verbose);
                order_for_change.write().unwrap().push("on_change".into());
            })),
        );
        store.subscribe(Arc::new(move || {
            order_for_listener.write().unwrap().push("listener".into());
        }));

        store.replace_with(|state| state.verbose = true);
        assert_eq!(&*order.read().unwrap(), &["on_change", "listener"]);
    }

    #[test]
    fn update_effects_can_reenter_without_deadlock() {
        let store = AppStore::new(AppState::default(), None);
        let store_for_listener = store.clone();
        store.subscribe(Arc::new(move || {
            // Re-entrant write from a listener — CC allows synchronous
            // recursion; the re-entrant turn (clause 4) keeps it safe.
            if store_for_listener.get().expanded_view == ExpandedView::None
                && store_for_listener.get().verbose
            {
                store_for_listener.replace_with(|state| state.expanded_view = ExpandedView::Tasks);
            }
        }));

        store.replace_with(|state| state.verbose = true);
        assert_eq!(store.get().expanded_view, ExpandedView::Tasks);
    }

    #[test]
    fn unsubscribe_stops_notifications() {
        let notified = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);
        let handle = store.subscribe(counting_listener(&notified));

        store.replace_with(|state| state.verbose = true);
        handle.unsubscribe();
        store.replace_with(|state| state.verbose = false);
        assert_eq!(notified.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn get_returns_snapshot_unaffected_by_later_updates() {
        let store = AppStore::new(AppState::default(), None);
        let before = store.get();
        store.replace_with(|state| state.verbose = true);
        assert!(!before.verbose);
        assert!(store.get().verbose);
    }

    // ---- P3 B1: set_state / replace_with / revision ----

    #[test]
    fn set_state_same_returns_result_without_notify_or_revision_bump() {
        let notified = Arc::new(AtomicUsize::new(0));
        let on_change_runs = Arc::new(AtomicUsize::new(0));
        let on_change_runs_in_cb = on_change_runs.clone();
        let store = AppStore::new(
            AppState::default(),
            Some(Arc::new(move |_, _| {
                on_change_runs_in_cb.fetch_add(1, Ordering::SeqCst);
            })),
        );
        store.subscribe(counting_listener(&notified));

        let result = store.set_state(|_prev| UpdateDecision::Same(42));
        assert_eq!(result, 42);
        assert_eq!(notified.load(Ordering::SeqCst), 0);
        assert_eq!(on_change_runs.load(Ordering::SeqCst), 0);
        assert_eq!(store.revision(), 0);
    }

    #[test]
    fn set_state_replace_notifies_and_bumps_revision() {
        let notified = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);
        store.subscribe(counting_listener(&notified));

        let result = store.set_state(|prev| {
            let mut next = (**prev).clone();
            next.verbose = true;
            UpdateDecision::Replace {
                next: Arc::new(next),
                result: "installed",
            }
        });
        assert_eq!(result, "installed");
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert_eq!(store.revision(), 1);
        assert!(store.get().verbose);
    }

    #[test]
    fn set_state_replace_with_identical_root_arc_is_suppressed() {
        // Contract A clause 1: a Replace handing back the same root Arc is
        // defensively treated as Same (CC `Object.is(next, prev)`).
        let notified = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);
        store.subscribe(counting_listener(&notified));

        let result = store.set_state(|prev| UpdateDecision::Replace {
            next: Arc::clone(prev),
            result: 7,
        });
        assert_eq!(result, 7);
        assert_eq!(notified.load(Ordering::SeqCst), 0);
        assert_eq!(store.revision(), 0);
    }

    // ---- B2: Contract A clause 3 live-Set semantics (store.ts:26,29-32),
    // clause 5 panic boundaries, and the cross-thread turn (clause 4).

    #[test]
    fn unsubscribe_is_repeatable_noop_like_cc_closure() {
        let notified = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);
        let handle = store.subscribe(counting_listener(&notified));

        store.replace_with(|state| state.verbose = true);
        handle.unsubscribe();
        handle.unsubscribe(); // idempotent no-op (CC: double-invoked closure)
        store.replace_with(|state| state.verbose = false);
        assert_eq!(notified.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn duplicate_identity_subscribe_coalesces_to_one_entry_like_cc_set() {
        // CC Set add: same function identity coalesces into one entry; every
        // handle is the same stateless delete closure.
        let calls = Arc::new(AtomicUsize::new(0));
        let shared = counting_listener(&calls);
        let store = AppStore::new(AppState::default(), None);
        let first = store.subscribe(Arc::clone(&shared));
        let second = store.subscribe(Arc::clone(&shared));

        store.replace_with(|state| state.verbose = true);
        assert_eq!(calls.load(Ordering::SeqCst), 1); // CC Set: one entry

        first.unsubscribe(); // either handle removes the single entry
        store.replace_with(|state| state.verbose = false);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        second.unsubscribe(); // remaining handle: idempotent no-op
    }

    /// Contract A clause 3 verification scenario ①: an old handle deletes a
    /// RE-registration (u1=sub(f); u2=sub(f); u1(); sub(f); u2() ⇒ f gone).
    #[test]
    fn old_handle_deletes_re_registration_like_cc_stateless_closure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let shared = counting_listener(&calls);
        let store = AppStore::new(AppState::default(), None);
        let u1 = store.subscribe(Arc::clone(&shared));
        let u2 = store.subscribe(Arc::clone(&shared));

        u1.unsubscribe();
        store.subscribe(Arc::clone(&shared)); // re-registration
        u2.unsubscribe(); // stateless identity delete removes the re-added f

        store.replace_with(|state| state.verbose = true);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn listener_added_mid_pass_runs_at_end_of_same_pass_like_cc_live_set() {
        let order = Arc::new(RwLock::new(Vec::<&'static str>::new()));
        let registered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let store = AppStore::new(AppState::default(), None);
        let store_in_first = store.clone();
        let order_in_first = order.clone();
        let registered_in_first = registered.clone();
        store.subscribe(Arc::new(move || {
            order_in_first.write().unwrap().push("first");
            if !registered_in_first.swap(true, Ordering::SeqCst) {
                let order_in_late = order_in_first.clone();
                store_in_first.subscribe(Arc::new(move || {
                    order_in_late.write().unwrap().push("late");
                }));
            }
        }));
        let order_in_second = order.clone();
        store.subscribe(Arc::new(move || {
            order_in_second.write().unwrap().push("second");
        }));

        store.replace_with(|state| state.verbose = true);
        // CC live Set: the appended listener is visible before the pass ends.
        assert_eq!(&*order.read().unwrap(), &["first", "second", "late"]);
    }

    #[test]
    fn listener_can_unsubscribe_itself_mid_pass_and_stays_removed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);
        let handle_cell = Arc::new(Mutex::new(None::<Unsubscribe>));
        let handle_cell_in_listener = handle_cell.clone();
        let calls_in_listener = calls.clone();
        let handle = store.subscribe(Arc::new(move || {
            calls_in_listener.fetch_add(1, Ordering::SeqCst);
            if let Some(handle) = handle_cell_in_listener.lock().unwrap().as_ref() {
                handle.unsubscribe();
                handle.unsubscribe(); // repeatable no-op
            }
        }));
        *handle_cell.lock().unwrap() = Some(handle);

        store.replace_with(|state| state.verbose = true);
        store.replace_with(|state| state.verbose = false);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Contract A clause 3 scenario ②: the CURRENT listener removing and
    /// re-adding itself moves to the tail and is revisited — A,B,A (JS Set
    /// delete + add during iteration).
    #[test]
    fn current_listener_remove_re_add_is_revisited_at_tail() {
        let order = Arc::new(RwLock::new(Vec::<&'static str>::new()));
        let store = AppStore::new(AppState::default(), None);
        let self_arc_cell = Arc::new(Mutex::new(None::<Listener>));
        let did_readd = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let a: Listener = Arc::new({
            let order = order.clone();
            let store = store.clone();
            let self_arc_cell = self_arc_cell.clone();
            let did_readd = did_readd.clone();
            move || {
                order.write().unwrap().push("A");
                if !did_readd.swap(true, Ordering::SeqCst) {
                    let self_arc = self_arc_cell.lock().unwrap().clone().unwrap();
                    // delete(f) then add(f): entry moves to the Set tail.
                    store.subscribe(Arc::clone(&self_arc)).unsubscribe();
                    store.subscribe(self_arc);
                }
            }
        });
        *self_arc_cell.lock().unwrap() = Some(Arc::clone(&a));
        store.subscribe(a);
        store.subscribe(Arc::new({
            let order = order.clone();
            move || order.write().unwrap().push("B")
        }));

        store.replace_with(|state| state.verbose = true);
        assert_eq!(&*order.read().unwrap(), &["A", "B", "A"]);
    }

    /// Contract A clause 3 scenario ③: removing + re-adding an UNVISITED
    /// listener moves it to the tail — A,C,B.
    #[test]
    fn unvisited_listener_remove_re_add_moves_to_tail() {
        let order = Arc::new(RwLock::new(Vec::<&'static str>::new()));
        let store = AppStore::new(AppState::default(), None);
        let b: Listener = Arc::new({
            let order = order.clone();
            move || order.write().unwrap().push("B")
        });
        let b_for_a = Arc::clone(&b);
        let store_for_a = store.clone();
        store.subscribe(Arc::new({
            let order = order.clone();
            let moved = std::sync::atomic::AtomicBool::new(false);
            move || {
                order.write().unwrap().push("A");
                if !moved.swap(true, Ordering::SeqCst) {
                    store_for_a.subscribe(Arc::clone(&b_for_a)).unsubscribe();
                    store_for_a.subscribe(Arc::clone(&b_for_a));
                }
            }
        }));
        store.subscribe(b);
        store.subscribe(Arc::new({
            let order = order.clone();
            move || order.write().unwrap().push("C")
        }));

        store.replace_with(|state| state.verbose = true);
        assert_eq!(&*order.read().unwrap(), &["A", "C", "B"]);
    }

    /// Contract A clause 3 scenario ④: a listener appended and removed
    /// before its visit never runs.
    #[test]
    fn appended_then_removed_before_visit_never_runs() {
        let late_calls = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);
        let store_for_a = store.clone();
        let late = counting_listener(&late_calls);
        store.subscribe(Arc::new({
            let done = std::sync::atomic::AtomicBool::new(false);
            move || {
                if !done.swap(true, Ordering::SeqCst) {
                    let handle = store_for_a.subscribe(Arc::clone(&late));
                    handle.unsubscribe();
                }
            }
        }));

        store.replace_with(|state| state.verbose = true);
        assert_eq!(late_calls.load(Ordering::SeqCst), 0);
    }

    /// Contract A clause 3 scenario ⑤ (F-A2): registry mutations made by a
    /// NESTED pass are visible to the outer in-flight cursor. B unsubscribes
    /// C on its first call, which happens inside the nested pass; the outer
    /// pass must then skip C.
    #[test]
    fn nested_pass_registry_mutations_visible_to_outer_cursor() {
        let order = Arc::new(RwLock::new(Vec::<&'static str>::new()));
        let c_calls = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);

        let c: Listener = Arc::new({
            let c_calls = c_calls.clone();
            move || {
                c_calls.fetch_add(1, Ordering::SeqCst);
            }
        });
        let store_for_a = store.clone();
        store.subscribe(Arc::new({
            let order = order.clone();
            let nested = std::sync::atomic::AtomicBool::new(false);
            move || {
                order.write().unwrap().push("A");
                if !nested.swap(true, Ordering::SeqCst) {
                    // Nested synchronous pass (CC listener calling setState).
                    store_for_a.replace_with(|state| state.expanded_view = ExpandedView::Tasks);
                }
            }
        }));
        let c_for_b = Arc::clone(&c);
        let store_for_b = store.clone();
        store.subscribe(Arc::new({
            let order = order.clone();
            let removed = std::sync::atomic::AtomicBool::new(false);
            move || {
                order.write().unwrap().push("B");
                if !removed.swap(true, Ordering::SeqCst) {
                    // First B call happens inside the nested pass; the outer
                    // cursor must observe C's removal.
                    store_for_b.subscribe(Arc::clone(&c_for_b)).unsubscribe();
                }
            }
        }));
        store.subscribe(c);

        store.replace_with(|state| state.verbose = true);
        assert_eq!(
            c_calls.load(Ordering::SeqCst),
            0,
            "C removed by the nested pass"
        );
        assert_eq!(&*order.read().unwrap(), &["A", "A", "B", "B"]);
    }

    // ---- Clause 5: panic boundaries ----

    #[test]
    fn updater_panic_leaves_root_unchanged_and_turn_released() {
        let store = AppStore::new(AppState::default(), None);
        let store_for_panic = store.clone();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            store_for_panic.set_state::<()>(|_prev| panic!("updater boom"));
        }));
        assert!(unwound.is_err());
        assert!(!store.get().verbose);
        assert_eq!(store.revision(), 0);
        // Turn released on unwind: the store keeps working.
        store.replace_with(|state| state.verbose = true);
        assert!(store.get().verbose);
        assert_eq!(store.revision(), 1);
    }

    #[test]
    fn on_change_panic_keeps_new_root_and_skips_all_listeners() {
        let listener_calls = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(
            AppState::default(),
            Some(Arc::new(|new: &AppState, _old: &AppState| {
                if new.verbose {
                    panic!("on_change boom");
                }
            })),
        );
        store.subscribe(counting_listener(&listener_calls));

        let store_for_panic = store.clone();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            store_for_panic.replace_with(|state| state.verbose = true);
        }));
        assert!(unwound.is_err());
        // Clause 5: install precedes on_change — new root kept, revision
        // bumped, the entire listener pass skipped.
        assert!(store.get().verbose);
        assert_eq!(store.revision(), 1);
        assert_eq!(listener_calls.load(Ordering::SeqCst), 0);

        // No poisoning, turn released: the store keeps working (on_change
        // no longer panics once verbose is false).
        store.replace_with(|state| state.verbose = false);
        assert_eq!(listener_calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.revision(), 2);
    }

    #[test]
    fn listener_panic_keeps_new_root_and_skips_rest_of_pass() {
        let later_calls = Arc::new(AtomicUsize::new(0));
        let store = AppStore::new(AppState::default(), None);
        let panicker: Listener = Arc::new(|| panic!("listener boom"));
        let panic_handle = store.subscribe(Arc::clone(&panicker));
        store.subscribe(counting_listener(&later_calls));

        let store_for_panic = store.clone();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            store_for_panic.replace_with(|state| state.verbose = true);
        }));
        assert!(unwound.is_err());
        // New root kept (install precedes the pass), rest of the pass skipped.
        assert!(store.get().verbose);
        assert_eq!(store.revision(), 1);
        assert_eq!(later_calls.load(Ordering::SeqCst), 0);

        // No poisoning: the store stays fully usable after the unwind.
        panic_handle.unsubscribe();
        store.replace_with(|state| state.verbose = false);
        assert_eq!(later_calls.load(Ordering::SeqCst), 1);
    }

    /// Contract B clause 4: writes made before Provider adoption must not
    /// run the change pipeline (CC has no store/onChange before
    /// `AppStateProvider` mounts, AppState.tsx:73-78); binding at adoption
    /// makes every later install run it.
    #[test]
    fn on_change_is_silent_until_bound_at_adoption() {
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_in_cb = runs.clone();
        let store = AppStore::new(AppState::default(), None);

        // Pre-mount writer: installs a root, fires nothing.
        store.replace_with(|state| state.verbose = true);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert_eq!(store.revision(), 1, "the install itself still happens");
        assert!(store.get().verbose);

        store.bind_on_change(Arc::new(move |_new, _old| {
            runs_in_cb.fetch_add(1, Ordering::SeqCst);
        }));

        store.replace_with(|state| state.verbose = false);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    // ---- Clause 4: cross-thread turn ----

    #[test]
    fn cross_thread_set_state_serializes_via_turn() {
        // Read-modify-write accumulation: each updater parses the current
        // counter out of the root and writes back +1. Without the turn's
        // mutual exclusion two threads could read the same prev and lose an
        // increment (the final count would fall short), so the ==200
        // assertion actually falsifies a missing turn — unlike unique-value
        // writes, which any per-install lock would pass.
        fn increment(store: &AppStore) {
            store.replace_with(|state| {
                let current: u64 = state
                    .advisor_model
                    .as_deref()
                    .unwrap_or("0")
                    .parse()
                    .expect("counter text");
                state.advisor_model = Some((current + 1).to_string());
            });
        }

        let store = AppStore::new(AppState::default(), None);
        let store_for_thread = store.clone();
        let worker = std::thread::spawn(move || {
            for _ in 0..100 {
                increment(&store_for_thread);
            }
        });
        for _ in 0..100 {
            increment(&store);
        }
        worker.join().expect("worker thread");
        assert_eq!(store.get().advisor_model.as_deref(), Some("200"));
        assert_eq!(store.revision(), 200);
    }
}
