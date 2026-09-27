//! Maps to: CC `state/AppState.tsx` `AppStateProvider` — the layer that
//! binds the framework-agnostic store to the UI tree.
//!
//! In CC, the provider hands the (stable) store reference down via React
//! Context and consumers subscribe to slices with `useSyncExternalStore`.
//! The Rust bridge reproduces both halves with iocraft-native primitives:
//!
//! - the Provider publishes the stable store PLUS an [`AppStateFrameCarrier`]
//!   whose `(root, revision)` is pinned once per update pass, so every
//!   consumer in one frame projects the same root (Contract C clause 1);
//! - each `use_app_state` call site owns an [`AppStateSubscription`] — a
//!   retained hook holding one store listener and one `Waker`. The listener
//!   only wakes; the selector runs on the render task's poll and its result
//!   is compared against what the terminal last showed, so a store change no
//!   consumer selected schedules no frame at all (Contract D clauses 1-2, 4).
//!
//! The predecessor design — one App-level `State<u64>` bumped by a store
//! listener — is gone. `State::set` is a `try_write`, so a bump raced against
//! a render-side borrow was silently discarded and the frame was lost until
//! some later, unrelated write happened to succeed (Contract C clause 3
//! names this failure mode and forbids it).
//!
//! The headless path (print mode) never mounts this bridge; it uses
//! `AppStore` directly WITH the canonical on_change, mirroring CC
//! `main.tsx:3728-3731` `createStore(headlessInitialState, onChangeAppState)`
//! (see `query_engine.rs` store construction).

use iocraft::prelude::*;
use std::sync::Arc;

use super::app_state_store::AppState;
use super::store::{AppStore, OnChangeFn};

/// Maps to: CC `AppState.tsx:55` `HasAppStateContext` — the marker that makes
/// nesting detectable. Zero-field: presence is the whole signal.
#[derive(Clone, Copy, Debug)]
pub struct HasAppStateProvider;

/// One frame's fixed view of the store.
///
/// Rust-only, no CC counterpart, `L1 (iocraft coherent AppState frame
/// carrier)`. React's `useSyncExternalStore` prevents tearing during
/// concurrent rendering; iocraft reconciles in place on one thread, so there is
/// no concurrent render to tear. What can still tear is a frame that reads the
/// live root at several points while a tool thread installs a new one between
/// them — half the tree would render the old root and half the new. Pinning
/// one `(root, revision)` per frame removes that.
///
/// Frame N renders a consistent projection of whatever root it pinned; a write
/// arriving mid-frame is visible in frame N+1. That is the same guarantee CC
/// gives: Ink throttles and coalesces renders (`ink/reconciler.ts:247,304`,
/// `FRAME_INTERVAL_MS=16`), so terminal frames and store updates are not
/// one-to-one there either — the hard promise is intra-frame consistency plus
/// eventual convergence.
#[derive(Clone, Debug)]
pub struct AppStateFrame {
    pub root: super::store::AppStateRoot,
    pub revision: u64,
}

/// The per-frame carrier published to descendants.
///
/// Rust-only, `L1 (iocraft coherent AppState frame carrier)`. The handle is a
/// stable `Arc`, so the context value never changes identity and publishing it
/// costs the Provider no re-render; only the CONTENTS are swapped, once per
/// frame, before any descendant updates.
#[derive(Clone)]
pub struct AppStateFrameCarrier {
    inner: Arc<std::sync::RwLock<AppStateFrame>>,
}

impl AppStateFrameCarrier {
    fn new(frame: AppStateFrame) -> Self {
        Self {
            inner: Arc::new(std::sync::RwLock::new(frame)),
        }
    }

    /// Pin the store's current `(root, revision)` as this frame's view.
    /// Called once per frame from the Provider, before descendants update.
    fn install(&self, store: &AppStore) {
        let (root, revision) = store.snapshot();
        let mut slot = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = AppStateFrame { root, revision };
    }

    /// The root every render-side selector reads this frame.
    pub fn root(&self) -> super::store::AppStateRoot {
        self.frame().root
    }

    /// This frame's revision — the value a memo key or bailout must fold in.
    pub fn revision(&self) -> u64 {
        self.frame().revision
    }

    /// Root and revision as the ONE pair this frame was pinned to. Every
    /// subscription re-syncs its baseline from this, so "what the terminal
    /// last showed" and "the revision that produced it" can never disagree.
    fn frame(&self) -> AppStateFrame {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl std::fmt::Debug for AppStateFrameCarrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppStateFrameCarrier(..)")
    }
}

/// Refreshes the frame at the top of every update pass.
///
/// `pre_component_update` runs before the component's children update, and
/// iocraft reconciles top-down from the root, so installing here is strictly
/// before every descendant read. The Provider is the common ancestor of all
/// AppState consumers, which is what makes one install per frame sufficient.
struct FrameRefreshHook {
    store: AppStore,
    carrier: AppStateFrameCarrier,
}

impl iocraft::prelude::Hook for FrameRefreshHook {
    fn pre_component_update(&mut self, _updater: &mut iocraft::ComponentUpdater) {
        self.carrier.install(&self.store);
    }
}

/// Children thunk, mirroring `components/app.rs::AppChildren`.
#[derive(Clone)]
pub struct ProviderChildren(Arc<dyn Fn() -> AnyElement<'static> + Send + Sync>);

impl ProviderChildren {
    pub fn new(render: impl Fn() -> AnyElement<'static> + Send + Sync + 'static) -> Self {
        Self(Arc::new(render))
    }

    fn render(&self) -> AnyElement<'static> {
        (self.0)()
    }
}

impl Default for ProviderChildren {
    fn default() -> Self {
        Self::new(|| element! { Fragment }.into_any())
    }
}

/// Maps to: CC `AppState.tsx:49-53` `Props`.
///
/// `prebuilt_store` is the Rust-only adoption slot: the interactive launch
/// phase builds the store before mount (retained launch phase), so the
/// Provider adopts it instead of constructing one. Exactly one of
/// `prebuilt_store` / `initial_state` is used; both absent =
/// `AppState::default()` (CC `getDefaultAppState()`, :75).
#[derive(Default, Props)]
pub struct AppStateProviderProps {
    pub children: ProviderChildren,
    /// Maps to: CC `Props.initialState` (:51).
    pub initial_state: Option<AppState>,
    /// Maps to: CC `Props.onChangeAppState` (:52) — passed straight into
    /// `createStore` (:73-78). It is OPTIONAL at the source: `App.tsx:30`
    /// supplies `onChangeAppState` for the interactive root, while the
    /// auxiliary per-dialog providers (`interactiveHelpers.tsx:121-131`,
    /// `mcpServerApproval.tsx:29-45`) pass none and therefore run with no
    /// change pipeline at all. Binding one unconditionally would make an
    /// auxiliary default-state store persist settings the source never writes.
    pub on_change_app_state: Option<OnChangeFn>,
    /// Rust-only, `L1 (Prebuilt interactive AppStore provider adoption)`.
    pub prebuilt_store: Option<AppStore>,
}

/// Maps to: CC `AppState.tsx:57-121` `AppStateProvider`.
///
/// Owns the store's mount lifecycle: one-shot creation/adoption, the change
/// pipeline binding (CC binds it inside the `useState` initializer, :73-78),
/// the mount-only bypass-permissions reconciliation (:85-102), and the
/// provider-scoped settings subscription (:104-110). It is the SOLE provider
/// of the `AppStore` context — no ancestor may provide the same type
/// (Contract B clause 2).
#[component]
pub fn AppStateProvider(
    props: &AppStateProviderProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // CC :62-68 — nesting is detected FIRST and throws IMMEDIATELY, before a
    // store is created, a pipeline is bound, or a subscription is installed.
    // Panicking here cannot desynchronize hook order: `try_use_context` takes
    // `&self` and reads only the context stack (iocraft `use_context.rs:74-77`)
    // — it consumes no hook slot — and a panicking render has no successor
    // render whose order could disagree.
    if hooks.try_use_context::<HasAppStateProvider>().is_some() {
        panic!("AppStateProvider can not be nested within another AppStateProvider");
    }

    // CC :73-78 — created (or adopted) exactly once; the `use_state`
    // initializer runs only on the first render, so the context value is
    // stable and the Provider itself never re-renders its subtree.
    let store_state = hooks.use_state({
        let prebuilt = props.prebuilt_store.clone();
        let initial_state = props.initial_state.clone();
        let on_change = props.on_change_app_state.clone();
        move || match prebuilt {
            // CC hands `onChangeAppState` to `createStore`; a prebuilt store
            // binds it at adoption instead, which is the same instant in the
            // mount lifecycle (Contract B clause 4: pre-mount writes stay
            // silent). `bind_on_change` runs inside the store turn, so
            // adoption is a linearization point against in-flight writers.
            Some(store) => {
                if let Some(on_change) = on_change {
                    store.bind_on_change(on_change);
                }
                store
            }
            None => AppStore::new(initial_state.unwrap_or_default(), on_change),
        }
    });
    let store = store_state.read().clone();

    // CC :85-102 — mount-only repair for the race where remote settings load
    // BEFORE mount, so the settings-change notification fired with no
    // listener subscribed. Runs in a `use_state` initializer rather than
    // `use_effect`: iocraft effects run before the draw and have no cleanup,
    // so an effect here would repair the state one frame EARLIER than CC
    // (which repairs after commit). Initializer timing is the closer match
    // for a mount-once, no-cleanup repair.
    hooks.use_state({
        let store = store.clone();
        move || {
            let context = store.tool_permission_context();
            if context.is_bypass_permissions_mode_available
                && crate::utils::permissions::permission_setup::is_bypass_permissions_mode_disabled(
                )
            {
                tracing::debug!(
                    "Disabling bypass permissions mode on mount (remote settings loaded before mount)"
                );
                store.replace_with(|state| {
                    let disabled =
                        crate::utils::permissions::permission_setup::create_disabled_bypass_permissions_context(
                            &state.tool_permission_context,
                        );
                    state.set_tool_permission_context(disabled);
                });
            }
        }
    });

    // CC :104-110 — `const onSettingsChange = useEffectEvent(source =>
    // applySettingsChange(source, store.setState)); useSettingsChange(...)`.
    // The subscription mechanics (read-through, subscribe, cleanup) belong to
    // the hook's own owner, `hooks/use_settings_change.rs` ≙
    // `hooks/useSettingsChange.ts`; the Provider only supplies the handler.
    // Like CC's handler, the settings argument is ignored —
    // `apply_settings_change` re-reads what it needs.
    crate::hooks::use_settings_change::use_settings_change(&mut hooks, {
        let store = store.clone();
        move |source, _settings| {
            crate::utils::settings::apply_settings_change::apply_settings_change(source, &store);
        }
    });

    // Contract C clause 1: one `(root, revision)` fixed per frame, published
    // alongside the stable store. The carrier handle is created once, so the
    // context value never changes identity; `FrameRefreshHook` swaps its
    // contents at the top of each update pass, before any descendant reads.
    let frame_carrier = hooks
        .use_state({
            let store = store.clone();
            move || {
                let (root, revision) = store.snapshot();
                AppStateFrameCarrier::new(AppStateFrame { root, revision })
            }
        })
        .read()
        .clone();
    hooks.use_hook(|| FrameRefreshHook {
        store: store.clone(),
        carrier: frame_carrier.clone(),
    });

    // CC :112-120 hierarchy. MailboxProvider (:115) has no Rust counterpart
    // — the whole `context/mailbox.tsx` + `utils/mailbox.ts` +
    // `hooks/useMailboxBridge.ts` chain is unported (MODULE_MAP), so its
    // layer is recorded here rather than faked with an empty provider.
    // VoiceProvider (:116) is an ant-only DCE passthrough (:20-25) and is
    // deliberately not ported.
    element! {
        ContextProvider(value: Context::owned(HasAppStateProvider)) {
            ContextProvider(value: Context::owned(store)) {
                ContextProvider(value: Context::owned(frame_carrier)) {
                    #(props.children.render())
                }
            }
        }
    }
}

/// RAII retained subscription: dropping (component unmount reclaiming the
/// owning hook) unsubscribes, fixing the pre-B2 leak where the listener
/// outlived the mount.
struct StoreSubscription(crate::state::store::Unsubscribe);

impl Drop for StoreSubscription {
    fn drop(&mut self) {
        self.0.unsubscribe();
    }
}

/// The wake channel a store listener is allowed to touch.
///
/// Rust-only, no CC counterpart, `L1 (Selected AppState subscription
/// carrier)`. Contract C clause 3 forbids the version-counter shape this
/// replaces: that listener called `State::set` from whatever thread wrote the
/// store, and `State::set` is a `try_write` (`iocraft use_state.rs:203-207`)
/// that **silently discards** the write under borrow contention — a dropped
/// frame, with no retry until the next unrelated store change. A `Waker` has
/// no such failure mode: `wake()` is infallible and idempotent, and the slot
/// is re-armed by the next poll.
#[derive(Default)]
struct SelectorWake {
    waker: std::sync::Mutex<Option<std::task::Waker>>,
}

impl SelectorWake {
    fn arm(&self, waker: &std::task::Waker) {
        *self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(waker.clone());
    }

    /// The entire body of the store listener (Contract D clause 2): no
    /// selector runs on the producer thread, so a tool thread's write never
    /// pays for — or panics inside — a consumer's projection.
    fn wake(&self) {
        let waker = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// One `useAppState` call site's subscription.
///
/// Maps to: CC `AppState.tsx:150-172` — `useSyncExternalStore(store.subscribe,
/// () => selector(store.getState()))`. React subscribes **per call site** and
/// bails out when the selected value is unchanged; this hook is that same
/// contract expressed with iocraft's retained-hook API (`Hook::poll_change`).
///
/// Rust-only carrier name, `L1 (Selected AppState subscription carrier)`.
///
/// Two deliberate deviations from `useSyncExternalStore`, both recorded in the
/// P6 scout:
/// - equality is `T: PartialEq`, not CC's `Object.is`. Every selector in the
///   tree returns a scalar, an `Option<scalar>` or an `Arc` — CC's own rule
///   (`AppState.tsx:138-148`: "Do NOT return new objects from the selector …
///   For multiple independent fields, call the hook multiple times"), and the
///   last selector that constructed a struct was split into per-field calls in
///   the #24 batch. Where the two comparisons differ, Rust deduplicates a
///   render CC would perform — the safe direction, and the reason a caller may
///   still assemble a struct from several selected fields *outside* the
///   selector.
/// - the value returned to the caller comes from the FRAME (Contract C clause
///   1), while `poll_change` projects the LIVE root. The frame is always
///   ≥ the polled root, so a poll that scheduled a frame can only under-report,
///   never over-report, staleness; the baseline is re-synced on every render to
///   the value actually committed to the terminal.
struct AppStateSubscription<T> {
    /// `None` outside a provider — the hook still occupies its slot so the
    /// call index stays stable (iocraft resolves hooks positionally).
    store: Option<AppStore>,
    /// Re-boxed every render: a selector may close over props that changed.
    selector: Option<Box<dyn Fn(&AppState) -> T + Send>>,
    /// What the terminal last showed for this call site.
    last_rendered: Option<T>,
    /// The newest revision this subscription has already judged — either by
    /// rendering it or by polling it and finding nothing selected changed.
    considered_revision: u64,
    wake: Arc<SelectorWake>,
    _subscription: Option<StoreSubscription>,
}

impl<T> Default for AppStateSubscription<T> {
    fn default() -> Self {
        Self {
            store: None,
            selector: None,
            last_rendered: None,
            considered_revision: 0,
            wake: Arc::new(SelectorWake::default()),
            _subscription: None,
        }
    }
}

impl<T> AppStateSubscription<T> {
    /// Subscribe on the first render that sees a store. The provider's store
    /// context is stable for the life of the mount (CC `AppState.tsx:73-78`
    /// creates it in a `useState` initializer), so one subscription per call
    /// site is the whole lifecycle.
    fn attach(&mut self, store: Option<AppStore>) {
        if self.store.is_some() {
            return;
        }
        let Some(store) = store else { return };
        let wake = self.wake.clone();
        self._subscription = Some(StoreSubscription(
            store.subscribe(Arc::new(move || wake.wake())),
        ));
        self.store = Some(store);
    }

    /// Re-point the poll-side baseline at what this render commits.
    ///
    /// `poll_change` compares against the value the terminal actually shows —
    /// the `useSyncExternalStore` invariant — so this is the only place the
    /// baseline is written, and every render goes through it. The selector is
    /// re-taken here too: it may close over props that changed this pass.
    fn sync_rendered(
        &mut self,
        selector: Box<dyn Fn(&AppState) -> T + Send>,
        value: T,
        revision: u64,
    ) {
        self.last_rendered = Some(value);
        self.considered_revision = revision;
        self.selector = Some(selector);
    }
}

impl<T> iocraft::prelude::Hook for AppStateSubscription<T>
where
    T: PartialEq + Send + Unpin + 'static,
{
    fn poll_change(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<()> {
        use std::task::Poll;

        let me = self.get_mut();
        let (Some(store), Some(selector)) = (me.store.as_ref(), me.selector.as_ref()) else {
            return Poll::Pending;
        };
        // Register BEFORE the recheck. A write landing between the two either
        // consumes this waker (and re-polls us) or is visible to the snapshot
        // below — there is no interleaving in which the wake is lost, which is
        // exactly what the version counter could not promise.
        me.wake.arm(cx.waker());
        let (root, revision) = store.snapshot();
        if revision == me.considered_revision {
            return Poll::Pending;
        }
        me.considered_revision = revision;
        // The selector runs HERE — on the render task's poll, never on the
        // thread that wrote the store (Contract D clause 2).
        let next = selector(&root);
        if me.last_rendered.as_ref() == Some(&next) {
            // Contract D clause 4: the store moved but this call site's slice
            // did not, so no frame is scheduled on its account.
            return Poll::Pending;
        }
        Poll::Ready(())
    }
}

/// Maps to: CC `useAppState(selector)` (state/AppState.tsx:150). Reads a slice
/// of `AppState` and subscribes this call site to changes in that slice.
///
/// Contract C clause 1: the slice comes from the FRAME, not the live store, so
/// every consumer in one frame projects the same root even if a tool thread
/// installs a new one mid-pass. Imperative paths (query, tools) keep reading
/// the live store through [`use_app_state_store`] — that is the same split CC
/// has between `useAppState` (render) and `useAppStateStore` (non-React
/// callers, `AppState.tsx:183-185`).
///
/// Panics outside an `AppStateProvider`, matching CC's invariant; use
/// [`use_app_state_maybe_outside_of_provider`] where the provider may be
/// absent.
pub fn use_app_state<T>(
    hooks: &mut Hooks,
    selector: impl Fn(&super::app_state_store::AppState) -> T + Send + 'static,
) -> T
where
    T: PartialEq + Clone + Send + Unpin + 'static,
{
    use_app_state_maybe_outside_of_provider(hooks, selector)
        .expect("useAppState/useSetAppState cannot be called outside of an <AppStateProvider />")
}

/// This frame's root and revision, or `None` outside a Provider.
///
/// Falls back to one live-store snapshot when a context-only compatibility
/// mount supplies the store without the frame carrier. Production retained
/// rendering mounts `AppStateProvider`, so normal component reads use the
/// frame installed at the start of the update pass.
fn app_state_frame(hooks: &Hooks) -> Option<(super::store::AppStateRoot, u64)> {
    if let Some(carrier) = hooks.try_use_context::<AppStateFrameCarrier>() {
        let frame = carrier.frame();
        return Some((frame.root, frame.revision));
    }
    hooks
        .try_use_context::<AppStore>()
        .map(|store| store.snapshot())
}

/// Maps to: CC `AppState.tsx:183-185` `useAppStateStore` — the store itself,
/// with no subscription.
pub fn use_app_state_store(hooks: &mut Hooks) -> AppStore {
    hooks
        .try_use_context::<AppStore>()
        .expect("useAppState/useSetAppState cannot be called outside of an <AppStateProvider />")
        .clone()
}

/// Maps to: CC `AppState.tsx:174-178` `useSetAppState` — the write half,
/// with no subscription. Returns the store because Rust's write entry points
/// (`set_state` / `replace_with`) are inherent methods rather than a bare
/// `setState` function value.
pub fn use_set_app_state(hooks: &mut Hooks) -> AppStore {
    use_app_state_store(hooks)
}

/// Maps to: CC `useAppStateMaybeOutsideOfProvider(selector)`
/// (state/AppState.tsx:193). Returns `None` when no `AppStore` provider is
/// mounted (e.g. isolated component tests).
pub fn use_app_state_maybe_outside_of_provider<T>(
    hooks: &mut Hooks,
    selector: impl Fn(&super::app_state_store::AppState) -> T + Send + 'static,
) -> Option<T>
where
    T: PartialEq + Clone + Send + Unpin + 'static,
{
    // Same frame source as the strict hook (Contract C clause 1), so a
    // provider-optional consumer cannot observe a different root than its
    // siblings within one frame.
    let frame = app_state_frame(hooks);
    let store = hooks
        .try_use_context::<AppStore>()
        .map(|store| store.clone());
    // The hook slot is claimed unconditionally: iocraft resolves hooks by call
    // index and `downcast_mut`s, so a slot that appeared only when a provider
    // was present would shift every later hook in a provider-less mount.
    let subscription = hooks.use_hook(AppStateSubscription::<T>::default);
    subscription.attach(store);

    let selector: Box<dyn Fn(&AppState) -> T + Send> = Box::new(selector);
    let (root, revision) = frame?;
    let value = selector(root.as_ref());
    subscription.sync_rendered(selector, value.clone(), revision);
    Some(value)
}

// `useNotifications()` lives with its source owner,
// `crate::context::notifications` ≙ CC `context/notifications.tsx:46`. It was
// here only because the whole notifications family had been absorbed into
// `state/` (P6 G3 moved it back).

// `use_mcp` is DELETED (P6 G3b). It had no CC counterpart at all: the source's
// component-facing MCP surface is `useMcpReconnect()` + `useMcpToggleEnabled()`
// over the context `MCPConnectionManager` publishes
// (`services/mcp/MCPConnectionManager.tsx:30-46`). The invented writer existed
// only because that manager, though written, was never mounted — so there was
// no context to read. See `services/mcp/mcp_connection_manager.rs`.

// Contract C evidence index (added 2026-08-09, #40). PORTING.md states the L1
// classification carries an evidence obligation — "lossless wake 测试 + frame
// trace ≤1 帧" — and that MODULE_MAP stays `partial` until it is met. The tests
// below DO meet it, but none of them carries those words, so grepping the
// contract's terms returns nothing and the row's `done-renamed` reads
// unsupported. It is supported; here is the mapping:
//
//   frame trace ≤ 1 frame
//     - a_background_write_converges_within_one_frame
//     - a_background_write_converges_to_the_canvas
//   lossless wake
//     - a_write_from_another_thread_always_reaches_the_subscription
//     - a_write_that_changes_nothing_selected_schedules_no_frame
//     - the_selector_never_runs_on_the_writing_thread
//     - dropping_the_subscription_unsubscribes
//   per-frame fixed AppStateFrame {root, revision} (clause 1)
//     - one_frame_projects_one_root_even_when_a_write_lands_mid_pass
//     - the_next_frame_picks_up_the_write
//     - snapshot_pairs_root_and_revision
//
// Renaming a test in these groups without updating this index breaks the only
// link between the contract and its proof.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;
    use crate::state::store::AppStore;

    #[component]
    fn StoreProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let verbose = use_app_state(&mut hooks, |state| state.verbose);
        element! { Text(content: format!("verbose={verbose}")) }
    }

    /// Contract C clause 1: every consumer in one frame reads the SAME root,
    /// even when a write lands between two of their reads.
    ///
    /// The probe reads the slice, then installs a new root from inside its own
    /// render (standing in for a tool thread writing mid-pass), then reads
    /// again. Without the frame both reads would differ; with it they agree and
    /// the new value shows up in the next frame.
    #[test]
    fn one_frame_projects_one_root_even_when_a_write_lands_mid_pass() {
        #[component]
        fn TearProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let before = use_app_state(&mut hooks, |state| state.verbose);
            let store = use_app_state_store(&mut hooks);
            // A write from inside the pass — the shape a tool thread produces.
            store.replace_with(|state| state.verbose = true);
            let after = use_app_state(&mut hooks, |state| state.verbose);
            element! { Text(content: format!("before={before} after={after}")) }
        }

        let store = AppStore::new(AppState::default(), None);
        let text = element! {
            AppStateProvider(
                prebuilt_store: Some(store.clone()),
                children: ProviderChildren::new(|| element!(TearProbe).into_any()),
            )
        }
        .render(Some(40))
        .to_string();

        assert!(
            text.contains("before=false after=false"),
            "both reads must see the frame's pinned root, not the live one; canvas=\n{text}"
        );
        // The write itself was not suppressed — it lands for the NEXT frame.
        assert!(store.get().verbose);
    }

    /// Contract C clause 1, second half: the frame advances between frames, so
    /// a write made during frame N is what frame N+1 reads.
    #[test]
    fn the_next_frame_picks_up_the_write() {
        let store = AppStore::new(AppState::default(), None);
        store.replace_with(|state| state.verbose = true);

        let text = element! {
            AppStateProvider(
                prebuilt_store: Some(store.clone()),
                children: ProviderChildren::new(|| element!(StoreProbe).into_any()),
            )
        }
        .render(Some(40))
        .to_string();

        assert!(text.contains("verbose=true"), "canvas=\n{text}");
    }

    /// `snapshot()` pairs root and revision under one lock, which is what makes
    /// a frame's revision trustworthy. Reading them separately can straddle an
    /// install and report a new count for an old root.
    #[test]
    fn snapshot_pairs_root_and_revision() {
        let store = AppStore::new(AppState::default(), None);
        let (root, revision) = store.snapshot();
        assert!(!root.verbose);
        assert_eq!(revision, 0);

        store.replace_with(|state| state.verbose = true);
        let (root, revision) = store.snapshot();
        assert!(root.verbose);
        assert_eq!(revision, 1);
    }

    /// Maps to: CC `AppState.tsx:73-78` — the Provider adopts the prebuilt
    /// store and publishes it as the sole `AppStore` context.
    #[test]
    fn provider_adopts_prebuilt_store_and_publishes_context() {
        let store = AppStore::new(AppState::default(), None);
        store.replace_with(|state| state.verbose = true);

        let text = element! {
            AppStateProvider(
                prebuilt_store: Some(store.clone()),
                children: ProviderChildren::new(|| element!(StoreProbe).into_any()),
            )
        }
        .render(Some(40))
        .to_string();

        assert!(text.contains("verbose=true"), "canvas=\n{text}");
    }

    /// Contract B clause 4: adoption binds the supplied pipeline, so writes
    /// made after mount run `on_change` while pre-mount writes stayed silent.
    #[test]
    fn adoption_binds_the_supplied_change_pipeline() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let runs = Arc::new(AtomicUsize::new(0));
        let runs_in_cb = runs.clone();
        let on_change: OnChangeFn = Arc::new(move |_new, _old| {
            runs_in_cb.fetch_add(1, Ordering::SeqCst);
        });

        let store = AppStore::new(AppState::default(), None);
        // Pre-mount write: no pipeline yet.
        store.replace_with(|state| state.verbose = true);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "pre-mount writes are silent"
        );

        let _ = element! {
            AppStateProvider(
                prebuilt_store: Some(store.clone()),
                on_change_app_state: Some(on_change),
                children: ProviderChildren::default(),
            )
        }
        .render(Some(20))
        .to_string();

        store.replace_with(|state| state.verbose = false);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "post-adoption writes run the pipeline the Provider was given"
        );
    }

    /// Maps to: CC `AppState.tsx:73-78` — `createStore(initialState,
    /// onChangeAppState)` with `onChangeAppState` UNDEFINED. The auxiliary
    /// per-dialog providers (`interactiveHelpers.tsx:121-131`,
    /// `mcpServerApproval.tsx:29-45`) pass none, so their store has no change
    /// pipeline at all and never persists anything.
    #[test]
    fn provider_without_a_pipeline_prop_binds_nothing() {
        let store = AppStore::new(AppState::default(), None);

        let _ = element! {
            AppStateProvider(
                prebuilt_store: Some(store.clone()),
                children: ProviderChildren::default(),
            )
        }
        .render(Some(20))
        .to_string();

        // Nothing was bound at adoption, so the store is still bindable — a
        // second bind would `assert!` inside `bind_on_change` if the Provider
        // had installed a default pipeline of its own.
        store.bind_on_change(Arc::new(|_new, _old| {}));
    }

    /// Contract A clause 4 + Contract B clause 4: `bind_on_change` runs inside
    /// the store turn, so adoption linearizes against an in-flight writer.
    /// Without the turn, a pre-mount updater already inside `set_state` could
    /// have the pipeline bound underneath it and then fire `on_change` for a
    /// write that must stay silent.
    #[test]
    fn adoption_linearizes_against_an_in_flight_pre_mount_writer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;

        let fired_for_pre_mount_write = Arc::new(AtomicUsize::new(0));
        let fired_in_cb = fired_for_pre_mount_write.clone();
        let on_change: OnChangeFn = Arc::new(move |new: &AppState, _old: &AppState| {
            // `verbose = true` is the pre-mount writer's payload.
            if new.verbose {
                fired_in_cb.fetch_add(1, Ordering::SeqCst);
            }
        });

        let store = AppStore::new(AppState::default(), None);
        let (inside_updater, updater_entered) = mpsc::channel();
        let (release, wait_for_release) = mpsc::channel();

        let store_for_writer = store.clone();
        let writer = std::thread::spawn(move || {
            store_for_writer.replace_with(|state| {
                // Announce that we hold the turn, then stall inside it.
                inside_updater.send(()).expect("signal");
                wait_for_release.recv().expect("release");
                state.verbose = true;
            });
        });

        updater_entered.recv().expect("writer entered set_state");
        let store_for_binder = store.clone();
        let binder = std::thread::spawn(move || {
            // Blocks on the turn until the writer's whole pass completes.
            store_for_binder.bind_on_change(on_change);
        });

        release.send(()).expect("release the writer");
        writer.join().expect("writer");
        binder.join().expect("binder");

        assert_eq!(
            fired_for_pre_mount_write.load(Ordering::SeqCst),
            0,
            "the pre-mount write must not run a pipeline bound while it was in flight"
        );
        // The pipeline IS bound afterwards.
        store.replace_with(|state| state.verbose = true);
        assert_eq!(fired_for_pre_mount_write.load(Ordering::SeqCst), 1);
    }

    /// Maps to: CC `AppState.tsx:62-68` — nesting is a hard error with the
    /// source's verbatim message.
    #[test]
    fn nested_provider_panics_like_official() {
        let outcome = std::panic::catch_unwind(|| {
            element! {
                AppStateProvider(
                    children: ProviderChildren::new(|| {
                        element! {
                            AppStateProvider(children: ProviderChildren::default())
                        }
                        .into_any()
                    }),
                )
            }
            .render(Some(20))
            .to_string()
        });

        let panic = outcome.expect_err("nested providers must panic");
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(
            message.contains("AppStateProvider can not be nested within another AppStateProvider"),
            "message={message}"
        );
    }

    /// CC `:62-68` throws on nesting BEFORE `createStore` runs. Reusing the
    /// same prebuilt store in both providers must therefore surface the
    /// official nesting message — not `bind_on_change`'s "bound twice", which
    /// is what an inner provider that creates/binds before checking would hit.
    #[test]
    fn nested_provider_sharing_a_store_reports_nesting_not_double_bind() {
        let store = AppStore::new(AppState::default(), None);
        let inner_store = store.clone();
        let outcome = std::panic::catch_unwind(move || {
            element! {
                AppStateProvider(
                    prebuilt_store: Some(store.clone()),
                    on_change_app_state: Some(Arc::new(|_: &AppState, _: &AppState| {}) as OnChangeFn),
                    children: ProviderChildren::new(move || {
                        element! {
                            AppStateProvider(
                                prebuilt_store: Some(inner_store.clone()),
                                on_change_app_state: Some(
                                    Arc::new(|_: &AppState, _: &AppState| {}) as OnChangeFn
                                ),
                                children: ProviderChildren::default(),
                            )
                        }
                        .into_any()
                    }),
                )
            }
            .render(Some(20))
            .to_string()
        });

        let panic = outcome.expect_err("nested providers must panic");
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(
            message.contains("AppStateProvider can not be nested within another AppStateProvider"),
            "message={message}"
        );
    }

    /// Maps to: CC `AppState.tsx:75` — no prebuilt store and no
    /// `initialState` falls back to the default state.
    #[test]
    fn provider_without_inputs_creates_default_state_store() {
        let text = element! {
            AppStateProvider(
                children: ProviderChildren::new(|| element!(StoreProbe).into_any()),
            )
        }
        .render(Some(40))
        .to_string();

        assert!(text.contains("verbose=false"), "canvas=\n{text}");
    }

    // ---- P6 G2: per-call-site subscription (Contract C clause 3, D 1-2/4) ----

    /// Drive one subscription's `poll_change` by hand.
    ///
    /// The waker counts wakes, so the same probe answers both questions a
    /// subscription has to get right: did the producer thread reach us at all
    /// (C clause 3), and did we schedule a frame for it (D clause 4).
    struct WakeProbe {
        wakes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl std::task::Wake for WakeProbe {
        fn wake(self: Arc<Self>) {
            self.wakes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A subscription left exactly as one render would leave it.
    ///
    /// Both steps are the production entry points — `attach` and
    /// `sync_rendered` are what `use_app_state_maybe_outside_of_provider`
    /// calls — so this fixture cannot drift away from the real render path.
    /// Only the surrounding render loop is stubbed, which is the point: it is
    /// what makes `poll_change` observable one call at a time.
    fn rendered_subscription<T>(
        store: &AppStore,
        selector: impl Fn(&AppState) -> T + Send + 'static,
    ) -> AppStateSubscription<T> {
        let mut subscription = AppStateSubscription::<T>::default();
        subscription.attach(Some(store.clone()));
        let (root, revision) = store.snapshot();
        let selector: Box<dyn Fn(&AppState) -> T + Send> = Box::new(selector);
        let value = selector(root.as_ref());
        subscription.sync_rendered(selector, value, revision);
        subscription
    }

    fn poll_once<T>(
        subscription: &mut AppStateSubscription<T>,
        waker: &std::task::Waker,
    ) -> std::task::Poll<()>
    where
        T: PartialEq + Send + Unpin + 'static,
    {
        use iocraft::prelude::Hook;
        let mut cx = std::task::Context::from_waker(waker);
        std::pin::Pin::new(subscription).poll_change(&mut cx)
    }

    /// Contract C clause 3 — the defect this replaced. The predecessor bumped
    /// an iocraft `State<u64>` from the writing thread, and `State::set` is a
    /// `try_write` that silently discards under borrow contention: the frame
    /// was lost with no retry. A `Waker` cannot fail that way, and here the
    /// write comes from another thread, which is where AppState writes come
    /// from in production (tool execution runs off the render thread).
    #[test]
    fn a_write_from_another_thread_always_reaches_the_subscription() {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waker = std::task::Waker::from(Arc::new(WakeProbe {
            wakes: wakes.clone(),
        }));

        let store = AppStore::new(AppState::default(), None);
        let mut subscription = rendered_subscription(&store, |state: &AppState| state.verbose);

        // Arms the waker and finds nothing new.
        assert!(poll_once(&mut subscription, &waker).is_pending());
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 0);

        let store_for_writer = store.clone();
        std::thread::spawn(move || store_for_writer.replace_with(|state| state.verbose = true))
            .join()
            .expect("writer thread");

        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the cross-thread write must wake the render task"
        );
        assert!(
            poll_once(&mut subscription, &waker).is_ready(),
            "and the woken poll must schedule a frame"
        );
    }

    /// Contract D clause 4: honest granularity. The store advances (CC's
    /// `{...prev}` spread installs a fresh root for every write, so the
    /// revision moves even when nothing this call site selected changed), but
    /// the subscription schedules no frame on its account.
    #[test]
    fn a_write_that_changes_nothing_selected_schedules_no_frame() {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waker = std::task::Waker::from(Arc::new(WakeProbe {
            wakes: wakes.clone(),
        }));

        let store = AppStore::new(AppState::default(), None);
        let mut subscription = rendered_subscription(&store, |state: &AppState| state.verbose);
        assert!(poll_once(&mut subscription, &waker).is_pending());

        // A field nobody here selected.
        store.replace_with(|state| state.status_line_text = Some("busy".into()));
        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the listener still fires — granularity is the consumer's job"
        );
        assert!(
            poll_once(&mut subscription, &waker).is_pending(),
            "…but the selected slice is unchanged, so no frame is scheduled"
        );

        // The selected field moving does schedule one.
        store.replace_with(|state| state.verbose = true);
        assert!(poll_once(&mut subscription, &waker).is_ready());
    }

    /// Contract D clause 2: the store listener's whole body is `waker.wake()`.
    /// A selector that panics can therefore never take down the thread that
    /// wrote the store — it runs on the render task's poll instead.
    #[test]
    fn the_selector_never_runs_on_the_writing_thread() {
        let waker = std::task::Waker::from(Arc::new(WakeProbe {
            wakes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }));
        let store = AppStore::new(AppState::default(), None);
        let mut subscription = rendered_subscription(&store, |state: &AppState| {
            assert!(
                !state.verbose,
                "selector ran on the writer's thread, not the render task"
            );
            state.verbose
        });
        assert!(poll_once(&mut subscription, &waker).is_pending());

        // The panicking projection would fire inside `replace_with` if the
        // listener ran selectors; it must survive untouched.
        store.replace_with(|state| state.verbose = true);

        // The panic surfaces on the POLL, which is the contract.
        let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            poll_once(&mut subscription, &waker)
        }));
        assert!(polled.is_err(), "the selector runs on the poll");
    }

    /// Contract D clause 1: the subscription is retained per call site and
    /// released with the component. Dropping the hook must remove its listener
    /// so an unmounted consumer stops costing every writer a callback.
    #[test]
    fn dropping_the_subscription_unsubscribes() {
        let store = AppStore::new(AppState::default(), None);
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waker = std::task::Waker::from(Arc::new(WakeProbe {
            wakes: wakes.clone(),
        }));
        let mut subscription = rendered_subscription(&store, |state: &AppState| state.verbose);
        assert!(poll_once(&mut subscription, &waker).is_pending());

        store.replace_with(|state| state.verbose = true);
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 1);

        drop(subscription);
        store.replace_with(|state| state.verbose = false);
        assert_eq!(
            wakes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an unmounted call site must not be notified"
        );
    }

    /// Contract C clause 5, the convergence half: a write from a non-render
    /// thread must reach the terminal within ONE frame of the frame that
    /// observed it — not merely "eventually".
    ///
    /// Counting canvases is the durable form of that obligation.
    /// `COMETIX_DEBUG_PROFILES=frame` measures frame DURATIONS, which says nothing
    /// about how many frames a value took to appear, and a hand-run trace
    /// cannot regress a future change. This can.
    ///
    /// The bound is 1, measured rather than guessed: the write is joined
    /// between two canvases, so it has fully landed before the render loop is
    /// polled again and the very next canvas must show it. A looser bound would
    /// let a regression to two frames pass silently, which is the difference
    /// between evidence and decoration.
    ///
    /// What this does NOT cover: a write landing in the MIDDLE of a render pass.
    /// That is `one_frame_projects_one_root_even_when_a_write_lands_mid_pass`,
    /// which asserts the other half — that such a write cannot tear the frame
    /// it raced, and surfaces in the next one.
    #[test]
    fn a_background_write_converges_within_one_frame() {
        use futures::stream::{self, StreamExt};
        use iocraft::prelude::*;
        use std::time::Duration;

        let store = AppStore::new(AppState::default(), None);
        let store_for_tree = store.clone();
        let store_for_writer = store.clone();

        let frames_after_write = futures::executor::block_on(async move {
            let mut app = element! {
                AppStateProvider(
                    prebuilt_store: Some(store_for_tree),
                    children: ProviderChildren::new(|| element!(StoreProbe).into_any()),
                )
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(Vec::<TerminalEvent>::new()))
                        .with_size(30, 3),
                ),
            );

            let mut wrote = false;
            let mut frames_after_write = 0usize;
            for _ in 0..24 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(50)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                let text = canvas.to_string();

                if !wrote {
                    if text.contains("verbose=false") {
                        let store = store_for_writer.clone();
                        std::thread::spawn(move || {
                            store.replace_with(|state| state.verbose = true)
                        })
                        .join()
                        .expect("writer thread");
                        wrote = true;
                    }
                    continue;
                }

                frames_after_write += 1;
                if text.contains("verbose=true") {
                    return Some(frames_after_write);
                }
            }
            None
        });

        let frames = frames_after_write.expect("the write never reached the canvas");
        assert!(
            frames <= 1,
            "cross-thread write took {frames} frames to converge; clause 5 allows exactly one \
             frame after the write"
        );
    }

    /// The same path without the frame accounting — kept because it fails with
    /// a readable canvas dump when convergence breaks outright, where the
    /// counting test only reports a number.
    #[test]
    fn a_background_write_converges_to_the_canvas() {
        use futures::stream::{self, StreamExt};
        use iocraft::prelude::*;
        use std::time::Duration;

        let store = AppStore::new(AppState::default(), None);
        let store_for_tree = store.clone();
        let store_for_writer = store.clone();

        let text = futures::executor::block_on(async move {
            let mut app = element! {
                AppStateProvider(
                    prebuilt_store: Some(store_for_tree),
                    children: ProviderChildren::new(|| element!(StoreProbe).into_any()),
                )
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(Vec::<TerminalEvent>::new()))
                        .with_size(30, 3),
                ),
            );

            let mut last = String::new();
            let mut wrote = false;
            for _ in 0..24 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(50)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                last = canvas.to_string();
                if !wrote && last.contains("verbose=false") {
                    // Off the render thread, exactly like a tool execution.
                    let store = store_for_writer.clone();
                    std::thread::spawn(move || store.replace_with(|state| state.verbose = true))
                        .join()
                        .expect("writer thread");
                    wrote = true;
                    continue;
                }
                if wrote && last.contains("verbose=true") {
                    break;
                }
            }
            last
        });

        assert!(text.contains("verbose=true"), "canvas=\n{text}");
    }

    /// The bridge contract without a render loop: a listener that bumps a
    /// counter fires once per installed root (post-flip: every fresh root,
    /// value-equal or not — CC `{...prev}` spread) and coalescing is left to
    /// iocraft's State semantics.
    #[test]
    fn listener_bump_pattern_fires_per_effective_update() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let store = AppStore::new(AppState::default(), None);
        let version = Arc::new(AtomicU64::new(0));
        let version_in_listener = version.clone();
        store.subscribe(Arc::new(move || {
            version_in_listener.fetch_add(1, Ordering::SeqCst);
        }));

        store.replace_with(|state| state.verbose = true);
        store.replace_with(|state| state.verbose = true); // value-equal → still notifies (CC spread)
        store.replace_with(|state| state.verbose = false);
        assert_eq!(
            version.load(Ordering::SeqCst),
            3,
            "post-flip: every fresh root notifies (CC {{...prev}}); coalescing is the consumer's job"
        );
    }
}
