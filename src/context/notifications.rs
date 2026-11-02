//! Maps to: CC `context/notifications.tsx` — the footer notification queue.
//!
//! CC keeps this in `context/`, not `state/`: the data lives in
//! `AppState.notifications`, but the queue policy (priority ordering, the
//! `shouldAdd` duplicate guard, fold callbacks, invalidation, the two-transition
//! add, the mount-time promote) belongs to this owner. Nyan had absorbed the
//! whole family into `state/mod.rs`, which is why CC's only `context/` file with
//! real logic had no Rust counterpart at all.
//!
//! Split out of `state/mod.rs` on 2026-08-04 (P6 G3, following the naming
//! batch). Behaviour is unchanged by the move.

use std::sync::{Arc, LazyLock, Mutex};

/// Maps to: CC `context/notifications.tsx:41` `DEFAULT_TIMEOUT_MS = 8000`.
const DEFAULT_NOTIFICATION_TIMEOUT_MS: u64 = 8_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotificationPriority {
    Immediate,
    High,
    #[default]
    Medium,
    Low,
}

impl NotificationPriority {
    fn rank(self) -> u8 {
        match self {
            Self::Immediate => 0,
            Self::High => 1,
            Self::Medium => 2,
            Self::Low => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationColor {
    Error,
    Warning,
    Success,
    Claude,
    Text,
    Ide,
    Suggestion,
    FastMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationFold {
    /// Maps to official notification `fold(accumulator, incoming)` producers
    /// that count repeated lifecycle events by rewriting the leading count.
    CountPrefix { singular: String, plural: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationSegment {
    pub text: String,
    pub color: Option<NotificationColor>,
    pub dim: bool,
}

impl NotificationSegment {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
            dim: false,
        }
    }

    pub fn with_color(mut self, color: NotificationColor) -> Self {
        self.color = Some(color);
        self
    }

    pub fn with_dim(mut self, dim: bool) -> Self {
        self.dim = dim;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub key: String,
    /// Plain visible text used for duplicate/fold signatures and text-only
    /// notifications. Segment notifications keep this as the concatenated
    /// visible text while rendering `segments` for per-span style parity.
    pub text: String,
    pub segments: Vec<NotificationSegment>,
    pub color: Option<NotificationColor>,
    pub priority: NotificationPriority,
    pub timeout_ms: Option<u64>,
    pub invalidates: Vec<String>,
    pub fold: Option<NotificationFold>,
}

impl Notification {
    pub fn text(
        key: impl Into<String>,
        text: impl Into<String>,
        priority: NotificationPriority,
    ) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
            segments: Vec::new(),
            color: None,
            priority,
            timeout_ms: None,
            invalidates: Vec::new(),
            fold: None,
        }
    }

    pub fn with_color(mut self, color: NotificationColor) -> Self {
        self.color = Some(color);
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    pub fn with_segments(mut self, segments: Vec<NotificationSegment>) -> Self {
        self.segments = segments;
        self
    }

    pub fn with_invalidates(mut self, invalidates: impl IntoIterator<Item = String>) -> Self {
        self.invalidates = invalidates.into_iter().collect();
        self
    }

    pub fn with_fold(mut self, fold: NotificationFold) -> Self {
        self.fold = Some(fold);
        self
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_NOTIFICATION_TIMEOUT_MS)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotificationsState {
    pub current: Option<Notification>,
    pub queue: Vec<Notification>,
}

impl NotificationsState {
    /// Returns whether anything was written (B3 flip-audit: the `false`
    /// branches are CC context/notifications.tsx `return prev` guards —
    /// duplicate add :219-225; the writer converts `false` to
    /// `UpdateDecision::Same`).
    pub(crate) fn add(
        &mut self,
        notification: Notification,
        mut set_timeout: impl FnMut(Option<Notification>, bool),
    ) -> bool {
        if notification.priority == NotificationPriority::Immediate {
            set_timeout(Some(notification.clone()), true);
            self.add_immediate(notification);
            return true;
        }

        if let Some(folded) = fold_matching_notification(self.current.as_ref(), &notification) {
            set_timeout(Some(folded.clone()), false);
            self.current = Some(folded);
            return true;
        }

        if let Some(index) = self
            .queue
            .iter()
            .position(|queued| queued.key == notification.key)
        {
            return if let Some(folded) =
                fold_matching_notification(self.queue.get(index), &notification)
            {
                self.queue[index] = folded;
                true
            } else {
                // CC :220-225 shouldAdd=false (queued duplicate, no fold).
                false
            };
        }

        if self
            .current
            .as_ref()
            .is_some_and(|current| current.key == notification.key)
        {
            // CC :223 shouldAdd=false (current-key duplicate).
            return false;
        }

        let invalidates = notification.invalidates.clone();
        if self
            .current
            .as_ref()
            .is_some_and(|current| invalidates.iter().any(|key| key == &current.key))
        {
            set_timeout(None, false);
            self.current = None;
        }
        self.queue.retain(|queued| {
            queued.priority != NotificationPriority::Immediate
                && !invalidates.iter().any(|key| key == &queued.key)
        });
        self.queue.push(notification);
        // CC :250 returns here; the promote is the SEPARATE `processQueue()`
        // call at :252, i.e. a second store transition.
        true
    }

    /// Returns whether anything was written (CC :264-266 `if (!isCurrent &&
    /// !inQueue) return prev`).
    fn remove(&mut self, key: &str, mut clear_timeout: impl FnMut()) -> bool {
        let is_current = self
            .current
            .as_ref()
            .is_some_and(|current| current.key == key);
        let in_queue = self.queue.iter().any(|queued| queued.key == key);
        if !is_current && !in_queue {
            return false;
        }
        if is_current {
            clear_timeout();
            self.current = None;
        }
        self.queue.retain(|queued| queued.key != key);
        // CC :281 returns here; `processQueue()` at :283 is a second
        // transition.
        true
    }

    /// CC's timer callback (:64-79) and `removeNotification` clear `current`
    /// and return; the promote is the trailing `processQueue()` call, i.e. a
    /// separate transition. This mirrors the clear half only.
    pub fn clear_current(&mut self) {
        self.current = None;
    }

    /// Maps to: CC timer callbacks :64-79,108-126,167-181. Source guards
    /// by key; the real cancellable timer handle protects replacement timers.
    fn clear_current_for_timeout(&mut self, key: &str, invalidates: &[String]) -> bool {
        if !self
            .current
            .as_ref()
            .is_some_and(|current| current.key == key)
        {
            return false;
        }
        self.queue
            .retain(|queued| !invalidates.contains(&queued.key));
        self.clear_current();
        true
    }

    /// Maps to: CC `processQueue` (context/notifications.tsx:55-89) — its own
    /// `setAppState`, with the source guard `if (prev.notifications.current
    /// !== null || !next) return prev` (:57-60). Returns whether it wrote, so
    /// the writer can convert a no-op into `UpdateDecision::Same`.
    fn process_next(&mut self, mut set_timeout: impl FnMut(Notification)) -> bool {
        if self.current.is_some() {
            return false;
        }
        let Some(index) = next_notification_index(&self.queue) else {
            return false;
        };
        set_timeout(self.queue[index].clone());
        self.current = Some(self.queue.remove(index));
        true
    }

    fn add_immediate(&mut self, notification: Notification) {
        let invalidates = notification.invalidates.clone();
        let previous_current = self.current.take();
        let mut next_queue = Vec::new();

        if let Some(current) = previous_current
            && current.priority != NotificationPriority::Immediate
            && !invalidates.iter().any(|key| key == &current.key)
        {
            next_queue.push(current);
        }

        next_queue.extend(self.queue.drain(..).filter(|queued| {
            queued.priority != NotificationPriority::Immediate
                && !invalidates.iter().any(|key| key == &queued.key)
        }));

        self.current = Some(notification);
        self.queue = next_queue;
    }
}

pub fn next_notification_index(queue: &[Notification]) -> Option<usize> {
    queue
        .iter()
        .enumerate()
        .min_by_key(|(_, notification)| notification.priority.rank())
        .map(|(index, _)| index)
}

fn fold_matching_notification(
    accumulator: Option<&Notification>,
    incoming: &Notification,
) -> Option<Notification> {
    let accumulator = accumulator?;
    if accumulator.key != incoming.key {
        return None;
    }
    let fold = incoming.fold.as_ref()?;
    Some(match fold {
        NotificationFold::CountPrefix { singular, plural } => {
            let count = notification_count_prefix(accumulator).saturating_add(1);
            let mut folded = incoming.clone();
            folded.text = count_prefix_text(count, singular, plural);
            folded
        }
    })
}

fn notification_count_prefix(notification: &Notification) -> u32 {
    notification
        .text
        .split_whitespace()
        .next()
        .and_then(|prefix| prefix.parse::<u32>().ok())
        .unwrap_or(1)
}

pub fn count_prefix_text(count: u32, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// Maps to: CC context/notifications.tsx:44 `currentTimeoutId`.
struct NotificationTimeout {
    abort: futures::future::AbortHandle,
    identity: Arc<()>,
}
static CURRENT_TIMEOUT: LazyLock<Mutex<Option<NotificationTimeout>>> =
    LazyLock::new(|| Mutex::new(None));

/// AppStore-backed notifications writer. Maps to: CC `useNotifications()`
/// (context/notifications.tsx). The data lives in `AppState.notifications`;
/// this is a write helper obtained via `app_state::use_notifications`, NOT
/// an independently provided context source (nothing `ContextProvider`s it).
#[derive(Clone)]
pub struct NotificationsWriter {
    store: crate::state::store::AppStore,
}

impl NotificationsWriter {
    pub fn new(store: crate::state::store::AppStore) -> Self {
        Self { store }
    }

    /// Test-only reader. CC's tests observe the store; there is no such
    /// accessor on `useNotifications`'s return value (`:298`).
    #[cfg(test)]
    pub(crate) fn state(&self) -> NotificationsState {
        (*self.store.get().notifications).clone()
    }

    /// B3 flip-audit: the closure returns whether anything was written — the
    /// `false` branches are CC context/notifications.tsx `return prev` guards
    /// converted to `UpdateDecision::Same` from the source branch (never
    /// inferred from generic equality).
    fn mutate_guarded(&self, f: impl FnOnce(&mut NotificationsState) -> bool) {
        self.store.set_state(|prev| {
            let mut notifications = (*prev.notifications).clone();
            if !f(&mut notifications) {
                return crate::state::store::UpdateDecision::Same(());
            }
            let mut next = (**prev).clone();
            next.notifications = std::sync::Arc::new(notifications);
            crate::state::store::UpdateDecision::Replace {
                next: std::sync::Arc::new(next),
                result: (),
            }
        });
    }

    /// Maps to: CC `processQueue` (context/notifications.tsx:55-89) as its OWN
    /// `setAppState`. Every public writer below ends with this call, exactly
    /// like the source's trailing `processQueue()` (:252, :283) and the timer
    /// callback's (:77) — so a promote is a SECOND store transition, with its
    /// own revision bump and listener pass, never folded into the mutation.
    ///
    /// Restored 2026-08-03 (P5 re-review). The previous shape called
    /// `process_next` inside the mutation and installed a single root; the end
    /// state matched, but post-B3 every effective install notifies, so CC
    /// produces two revisions and two listener passes where Rust produced one,
    /// and CC's intermediate frame (queue updated, `current` not yet promoted)
    /// was never observable. Under Contract D that frame is a real slice a
    /// selector can be subscribed to.
    ///
    /// Private, like CC's: `processQueue` is a `useCallback` local to
    /// `useNotifications` (`:54`), never part of what the hook returns
    /// (`:298`).
    fn process_queue(&self) {
        self.mutate_guarded(|notifications| {
            notifications.process_next(|next| self.set_current_timeout(Some(next), false))
        });
    }

    /// Maps to: CC `context/notifications.tsx:292-296` — the mount-only effect
    ///
    /// ```js
    /// useEffect(() => {
    ///   if (store.getState().notifications.queue.length > 0) { processQueue() }
    /// }, [])
    /// ```
    ///
    /// This is the ONLY thing that promotes the startup seed. CC seeds
    /// `notifications: { current: null, queue: initialNotifications }`
    /// (`main.tsx:4107-4110`), so without this call the launch-time rows
    /// (`cli-permission-mode`, the statusline trust block, settings errors)
    /// sit in the queue until some unrelated add/remove happens to run
    /// `processQueue`. Splitting the promote out of the mutation (P5
    /// re-review) is only correct once this exists — the earlier comments
    /// claiming "promotion is the mounted tree's processQueue()" described a
    /// mechanism that had no Rust counterpart yet.
    ///
    /// The source reads the store imperatively here rather than subscribing,
    /// with the reason stated at `:288-291`: a subscription inside a
    /// mount-only effect would be vestigial and would re-render every caller
    /// on queue changes.
    ///
    /// Private, and driven from [`use_notifications`] — CC's effect is inside
    /// the hook, so no caller ever invokes this. It used to be `pub` with one
    /// caller in `screens/repl.rs`, which put a mount effect in a consumer
    /// that CC's REPL does not have.
    fn promote_startup_queue_on_mount(&self) {
        if self.store.get().notifications.queue.is_empty() {
            return;
        }
        self.process_queue();
    }

    pub fn add_notification(&mut self, notification: Notification) {
        // CC :198-250 — the mutation transition.
        self.mutate_guarded(|notifications| {
            notifications.add(notification, |next, immediate| {
                self.set_current_timeout(next, immediate)
            })
        });
        // CC :252 — "Process queue after adding the notification".
        self.process_queue();
    }

    pub fn remove_notification(&mut self, key: &str) {
        // CC :257-281 — the mutation transition.
        self.mutate_guarded(|notifications| {
            notifications.remove(key, || self.set_current_timeout(None, false))
        });
        // CC :283.
        self.process_queue();
    }

    /// Native timer-handle transport for source module `currentTimeoutId`.
    /// All source timeout branches call this while holding the store mutation;
    /// callback admission takes the same store→timer order, so reset/cancel
    /// cannot race an old same-key callback into clearing the replacement.
    fn set_current_timeout(&self, notification: Option<Notification>, immediate: bool) {
        let mut current = CURRENT_TIMEOUT.lock().expect("notification timeout lock");
        if let Some(previous) = current.take() {
            previous.abort.abort();
        }
        let Some(notification) = notification else {
            return;
        };
        let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work()
            .expect("notification timers require the process-lifetime runtime");
        let (abort, registration) = futures::future::AbortHandle::new_pair();
        let identity = Arc::new(());
        *current = Some(NotificationTimeout {
            abort,
            identity: identity.clone(),
        });
        let writer = self.clone();
        runtime.spawn(async move {
            let _ = futures::future::Abortable::new(
                async move {
                    let delay = notification.timeout_ms();
                    let delay = if delay == 0 || delay > i32::MAX as u64 {
                        1
                    } else {
                        delay
                    };
                    futures_timer::Delay::new(std::time::Duration::from_millis(delay)).await;
                    let mut admitted = false;
                    writer.mutate_guarded(|state| {
                        let mut current =
                            CURRENT_TIMEOUT.lock().expect("notification timeout lock");
                        if !current
                            .as_ref()
                            .is_some_and(|timer| Arc::ptr_eq(&timer.identity, &identity))
                        {
                            return false;
                        }
                        *current = None;
                        admitted = true;
                        state.clear_current_for_timeout(
                            &notification.key,
                            if immediate {
                                &notification.invalidates
                            } else {
                                &[]
                            },
                        )
                    });
                    if admitted {
                        writer.process_queue();
                    }
                },
                registration,
            )
            .await;
        });
    }
}

/// Maps to: CC `context/notifications.tsx:46-49` `useNotifications()`.
///
/// STRICT, like the source: CC builds this on `useAppStateStore()` +
/// `useSetAppState()` (`:50-51`), both of which throw outside an
/// `AppStateProvider`. The predecessor returned `Option<NotificationsWriter>`
/// via `try_use_context`, which turned a missing provider into a component
/// that silently swallowed every notification it tried to raise — a whole
/// class of bug the source cannot have (P6 G3, Contract D clause 5).
///
/// The writer keeps queue/timer maintenance private in this source owner;
/// component unmount does not own or abort the process-lifetime timer.
pub fn use_notifications(hooks: &mut iocraft::prelude::Hooks) -> NotificationsWriter {
    use iocraft::prelude::UseState;

    let writer = NotificationsWriter::new(crate::state::app_state::use_app_state_store(hooks));
    // CC `:292-296` — the mount-only `useEffect` that promotes the startup
    // seed. It belongs to the hook, so every mount of every caller carries it
    // and no consumer has to remember to. A `use_state` initializer is the
    // iocraft mount-once slot (initializers run on the first render only);
    // an effect would run one frame earlier than CC's post-commit effect.
    hooks.use_state({
        let writer = writer.clone();
        move || writer.promote_startup_queue_on_mount()
    });
    writer
}

#[cfg(test)]
mod tests {
    use super::*;
    /// CC has no standalone notifications-state object: `useNotifications`
    /// owns both the mutation `setAppState` and the trailing `processQueue()`
    /// (context/notifications.tsx:252, :283, :77). The writer is therefore the
    /// only level at which the source's transition sequence exists, so these
    /// tests drive it rather than the internal state struct.
    fn notifications_writer(
        initial: NotificationsState,
    ) -> (crate::state::store::AppStore, NotificationsWriter) {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        store.replace_with(|app| app.notifications = Arc::new(initial));
        let writer = NotificationsWriter::new(store.clone());
        (store, writer)
    }

    #[test]
    fn notifications_choose_highest_priority_with_stable_ties() {
        let queue = vec![
            Notification::text("low", "low", NotificationPriority::Low),
            Notification::text("high-a", "high a", NotificationPriority::High),
            Notification::text("high-b", "high b", NotificationPriority::High),
        ];

        assert_eq!(next_notification_index(&queue), Some(1));
    }

    #[test]
    fn immediate_notification_requeues_non_immediate_current_and_invalidates_keys() {
        let mut state = NotificationsState::default();
        state.add(
            Notification::text("current", "current", NotificationPriority::Medium),
            |_, _| {},
        );
        state.add(
            Notification::text("old", "old", NotificationPriority::Low),
            |_, _| {},
        );
        state.add(
            Notification::text("now", "now", NotificationPriority::Immediate)
                .with_invalidates(["old".to_string()]),
            |_, _| {},
        );

        assert_eq!(state.current.as_ref().map(|n| n.key.as_str()), Some("now"));
        assert_eq!(
            state
                .queue
                .iter()
                .map(|notification| notification.key.as_str())
                .collect::<Vec<_>>(),
            vec!["current"]
        );
    }

    #[test]
    fn immediate_notification_keeps_same_key_non_immediate_entries_like_official() {
        let mut state = NotificationsState {
            current: Some(Notification::text(
                "shared",
                "previous current",
                NotificationPriority::Medium,
            )),
            queue: vec![Notification::text(
                "shared",
                "previous queued",
                NotificationPriority::Low,
            )],
        };

        state.add(
            Notification::text("shared", "immediate", NotificationPriority::Immediate),
            |_, _| {},
        );

        assert_eq!(
            state
                .current
                .as_ref()
                .map(|notification| notification.text.as_str()),
            Some("immediate")
        );
        assert_eq!(
            state
                .queue
                .iter()
                .map(|notification| notification.text.as_str())
                .collect::<Vec<_>>(),
            vec!["previous current", "previous queued"]
        );
    }

    /// Maps to: CC `addNotification` (context/notifications.tsx:198-252) —
    /// the mutation `setAppState` at :199-250 and the `processQueue()` at :252
    /// are TWO store transitions. Listeners and `revision` observe both, and
    /// the intermediate root (notification queued, `current` not yet promoted)
    /// is a real frame a Contract D selector can be subscribed to.
    ///
    /// Maps to: CC `context/notifications.tsx:292-296` — the mount-only
    /// `processQueue()` that promotes the startup seed.
    ///
    /// This is the regression the transition split introduced and nothing
    /// caught: with the promote moved out of the mutation, the seeded queue
    /// from `main.tsx:4107-4110` had no counterpart to promote it, so the
    /// launch-time rows would sit invisible until an unrelated add or remove
    /// happened to run `process_queue`.
    #[test]
    fn mount_promotes_the_startup_queue_like_official() {
        let (store, writer) = notifications_writer(NotificationsState {
            current: None,
            queue: vec![
                Notification::text("cli-permission-mode", "seeded", NotificationPriority::High),
                Notification::text("settings-errors", "also seeded", NotificationPriority::Low),
            ],
        });
        let baseline = store.revision();

        writer.promote_startup_queue_on_mount();

        let state = writer.state();
        assert_eq!(
            state.current.as_ref().map(|n| n.key.as_str()),
            Some("cli-permission-mode"),
            "the highest-priority seeded row must be promoted at mount"
        );
        assert_eq!(state.queue.len(), 1, "the rest stays queued");
        assert_eq!(store.revision() - baseline, 1);

        // CC guards on `queue.length > 0`, so a second mount-time call with a
        // filled `current` is a Same — no revision bump.
        let after = store.revision();
        writer.promote_startup_queue_on_mount();
        assert_eq!(store.revision(), after);
    }

    /// CC `:293` guards on `queue.length > 0`, so an empty seed does nothing.
    #[test]
    fn mount_promote_is_a_noop_without_a_seeded_queue() {
        let (store, writer) = notifications_writer(NotificationsState::default());
        let baseline = store.revision();
        writer.promote_startup_queue_on_mount();
        assert_eq!(store.revision(), baseline);
        assert!(writer.state().current.is_none());
    }

    /// This is the test the previous folded implementation could not have
    /// passed: it asserted only the end state, which folding preserved.
    #[test]
    fn add_produces_the_two_store_transitions_cc_produces() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (store, mut writer) = notifications_writer(NotificationsState::default());
        let baseline = store.revision();

        // Record the `current` slice at every listener pass.
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Option<String>>::new()));
        let seen_in_listener = seen.clone();
        let store_in_listener = store.clone();
        store.subscribe(Arc::new(move || {
            let current = store_in_listener
                .get()
                .notifications
                .current
                .as_ref()
                .map(|n| n.key.clone());
            seen_in_listener.lock().unwrap().push(current);
        }));

        writer.add_notification(Notification::text(
            "first",
            "first",
            NotificationPriority::Low,
        ));

        assert_eq!(
            store.revision() - baseline,
            2,
            "CC :250 mutation + :252 processQueue = two transitions"
        );
        assert_eq!(
            &*seen.lock().unwrap(),
            &[None, Some("first".to_string())],
            "the intermediate frame (queued, not yet promoted) must be observable"
        );

        // CC's `processQueue` guard (:57-60) — with `current` already set, the
        // trailing call returns `prev`, so a second add is ONE transition.
        let before_second = store.revision();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_listener = calls.clone();
        store.subscribe(Arc::new(move || {
            calls_in_listener.fetch_add(1, Ordering::SeqCst);
        }));
        writer.add_notification(Notification::text(
            "second",
            "second",
            NotificationPriority::Low,
        ));
        assert_eq!(
            store.revision() - before_second,
            1,
            "processQueue returns prev when current is already set (CC :58)"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn removing_current_promotes_next_priority_notification() {
        let (_store, mut writer) = notifications_writer(NotificationsState::default());
        writer.add_notification(Notification::text(
            "medium",
            "medium",
            NotificationPriority::Medium,
        ));
        writer.add_notification(Notification::text(
            "high",
            "high",
            NotificationPriority::High,
        ));
        writer.remove_notification("medium");

        assert_eq!(
            writer.state().current.as_ref().map(|n| n.key.as_str()),
            Some("high")
        );
    }

    /// Maps to: CC `context/notifications.tsx:219-225` — a non-immediate,
    /// non-fold add whose key matches the current notification or an entry
    /// already queued is dropped (`if (!shouldAdd) return prev`).
    #[test]
    fn duplicate_key_non_immediate_add_is_dropped_like_official_should_add_guard() {
        let (_store, mut writer) = notifications_writer(NotificationsState::default());
        writer.add_notification(Notification::text(
            "dup",
            "original",
            NotificationPriority::Low,
        ));
        assert_eq!(
            writer.state().current.as_ref().map(|n| n.text.as_str()),
            Some("original")
        );

        // Same key as current -> dropped, current text unchanged, queue empty.
        writer.add_notification(Notification::text(
            "dup",
            "replacement",
            NotificationPriority::Low,
        ));
        let state = writer.state();
        assert_eq!(
            state.current.as_ref().map(|n| n.text.as_str()),
            Some("original"),
            "current-key duplicate must be ignored (CC :223)"
        );
        assert!(state.queue.is_empty());

        // Same key as a queued entry -> dropped, queue length unchanged.
        writer.add_notification(Notification::text(
            "queued",
            "first",
            NotificationPriority::Low,
        ));
        assert_eq!(writer.state().queue.len(), 1);
        writer.add_notification(Notification::text(
            "queued",
            "second",
            NotificationPriority::Low,
        ));
        let state = writer.state();
        assert_eq!(
            state.queue.len(),
            1,
            "queued-key duplicate must be ignored (CC :220-222)"
        );
        assert_eq!(state.queue[0].text, "first");
    }

    #[test]
    fn folding_notification_updates_current_count_like_official_fold_callback() {
        let fold = NotificationFold::CountPrefix {
            singular: "agent spawned".to_string(),
            plural: "agents spawned".to_string(),
        };
        let (_store, mut writer) = notifications_writer(NotificationsState::default());
        writer.add_notification(
            Notification::text(
                "teammate-spawn",
                "1 agent spawned",
                NotificationPriority::Low,
            )
            .with_timeout_ms(5_000)
            .with_fold(fold.clone()),
        );
        writer.add_notification(
            Notification::text(
                "teammate-spawn",
                "1 agent spawned",
                NotificationPriority::Low,
            )
            .with_timeout_ms(5_000)
            .with_fold(fold),
        );

        let current = writer
            .state()
            .current
            .expect("first notification should be current");
        assert_eq!(current.text, "2 agents spawned");
        assert_eq!(current.timeout_ms, Some(5_000));
        assert!(current.fold.is_some());
    }

    #[test]
    fn notification_timeout_matches_official_same_key_reset_and_recreated_content() {
        // context/notifications.tsx:95-128 replaces the timer even when key
        // and text are identical; callback compares key, never text identity.
        let (store, mut writer) = notifications_writer(NotificationsState::default());
        let notification = Notification::text(
            "escape-again-to-clear",
            "Esc again to clear",
            NotificationPriority::Immediate,
        )
        .with_timeout_ms(1000);
        writer.add_notification(notification.clone());
        std::thread::sleep(std::time::Duration::from_millis(300));
        writer.add_notification(notification);
        std::thread::sleep(std::time::Duration::from_millis(750));
        assert_eq!(
            writer.state().current.as_ref().map(|n| n.text.as_str()),
            Some("Esc again to clear"),
            "original timer must have been cancelled"
        );
        store.replace_with(|app| {
            let mut notifications = (*app.notifications).clone();
            notifications.current.as_mut().unwrap().text = "re-created content".into();
            app.notifications = Arc::new(notifications);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while writer.state().current.is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            writer.state().current.is_none(),
            "source timer expires re-created content by key"
        );
    }

    #[test]
    fn immediate_timeout_clear_filters_invalidated_queue_like_official() {
        let (_store, mut writer) = notifications_writer(NotificationsState {
            current: Some(
                Notification::text("now", "Now", NotificationPriority::Immediate)
                    .with_invalidates(["stale".to_string()]),
            ),
            queue: vec![
                Notification::text("stale", "Stale", NotificationPriority::Low),
                Notification::text("next", "Next", NotificationPriority::Low),
            ],
        });

        writer
            .mutate_guarded(|state| state.clear_current_for_timeout("now", &["stale".to_string()]));
        writer.process_queue();

        let state = writer.state();
        assert_eq!(state.current.as_ref().map(|n| n.key.as_str()), Some("next"));
        assert!(state.queue.is_empty());
    }

    #[test]
    fn non_immediate_timeout_clear_keeps_invalidates_as_queue_metadata_only() {
        let (_store, mut writer) = notifications_writer(NotificationsState {
            current: Some(
                Notification::text("notice", "Notice", NotificationPriority::Low)
                    .with_invalidates(["queued".to_string()]),
            ),
            queue: vec![Notification::text(
                "queued",
                "Queued",
                NotificationPriority::Low,
            )],
        });

        writer.mutate_guarded(|state| state.clear_current_for_timeout("notice", &[]));
        writer.process_queue();

        let state = writer.state();
        assert_eq!(
            state.current.as_ref().map(|n| n.key.as_str()),
            Some("queued")
        );
        assert!(state.queue.is_empty());
    }

    #[test]
    fn folding_notification_updates_queued_count_without_promoting() {
        let fold = NotificationFold::CountPrefix {
            singular: "agent shut down".to_string(),
            plural: "agents shut down".to_string(),
        };
        let mut state = NotificationsState {
            current: Some(Notification::text(
                "current",
                "current",
                NotificationPriority::Medium,
            )),
            queue: vec![
                Notification::text(
                    "teammate-shutdown",
                    "1 agent shut down",
                    NotificationPriority::Low,
                )
                .with_timeout_ms(5_000)
                .with_fold(fold.clone()),
            ],
        };

        state.add(
            Notification::text(
                "teammate-shutdown",
                "1 agent shut down",
                NotificationPriority::Low,
            )
            .with_timeout_ms(5_000)
            .with_fold(fold),
            |_, _| {},
        );

        assert_eq!(
            state.current.as_ref().map(|n| n.key.as_str()),
            Some("current")
        );
        assert_eq!(state.queue.len(), 1);
        assert_eq!(state.queue[0].text, "2 agents shut down");
    }
}
