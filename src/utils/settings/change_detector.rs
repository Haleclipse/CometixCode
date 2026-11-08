//! Maps to: CC `utils/settings/changeDetector.ts`.
//!
//! The process-wide settings-change notifier: one change source feeds
//! [`fan_out`], which resets the settings cache exactly once and then emits
//! to every subscriber. Consumers (the `AppStateProvider` subscription, CC
//! `AppState.tsx:104-110`) must NOT reset the cache themselves — CC removed
//! those defensive resets because N listeners each clearing and re-reading
//! caused N full disk reloads per notification (`changeDetector.ts:420-436`).
//!
//! Change-source seam: CC watches with chokidar (`:103-146`) and layers on
//! `awaitWriteFinish` stabilization (:107-112), a deletion grace window that
//! absorbs delete-and-recreate (:330-360), internal-write suppression
//! (:284-286), an MDM registry/plist poll (:381-418), and ConfigChange hook
//! gating (:292-301). Nyan keeps the pre-existing 1s mtime poll as the
//! source; the difference is coarser detection latency and no
//! ConfigChange-hook gating. Ported deliberately as a subset — the
//! ownership shape (single fan-out, cache reset at the producer,
//! subscribe/unsubscribe) is what Contract B clause 3 needs.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::SystemTime;

use super::constants::SettingSource;
use super::settings_cache::reset_settings_cache;

/// Maps to: CC `settingsChanged` signal payload — `[source: SettingSource]`.
pub type SettingsChangeListener = Arc<dyn Fn(SettingSource) + Send + Sync>;

/// Maps to: CC's `subscribe` return value (`utils/signal.ts` unsubscribe
/// closure). Stateless identity delete, same discipline as the AppStore
/// listener registry.
pub struct SettingsChangeUnsubscribe {
    listener: SettingsChangeListener,
}

impl SettingsChangeUnsubscribe {
    pub fn unsubscribe(&self) {
        let mut state = detector();
        state
            .listeners
            .retain(|entry| !Arc::ptr_eq(&entry.listener, &self.listener));
    }
}

/// One registry entry. `generation` is the live-iteration cursor only — never
/// a removal key (removal is pure `Arc::ptr_eq` identity), the same discipline
/// Contract A clause 3 establishes for the AppStore registry.
struct ListenerEntry {
    generation: u64,
    listener: SettingsChangeListener,
}

#[derive(Default)]
struct DetectorState {
    listeners: Vec<ListenerEntry>,
    next_generation: u64,
    initialized: bool,
    disposed: bool,
}

fn detector() -> MutexGuard<'static, DetectorState> {
    static DETECTOR: OnceLock<Mutex<DetectorState>> = OnceLock::new();
    DETECTOR
        .get_or_init(|| Mutex::new(DetectorState::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Sources watched for changes. Maps to: CC `getWatchTargets` (:180-250),
/// which skips `flagSettings` (:194-196: CLI-provided, never changes in a
/// session, and may live in `$TMPDIR` alongside special files).
const WATCHED_SOURCES: [SettingSource; 3] = [
    SettingSource::User,
    SettingSource::Project,
    SettingSource::Local,
];

/// Maps to: CC `initialize()` (:84-146) — start the change source once.
/// Idempotent, and a no-op after [`dispose`], matching CC's
/// `if (initialized || disposed) return` (:86).
pub fn initialize() {
    {
        let mut state = detector();
        if state.initialized || state.disposed {
            return;
        }
        state.initialized = true;
    }

    tokio::spawn(async move {
        let mut last: Vec<Option<SystemTime>> = WATCHED_SOURCES.iter().map(|s| mtime(*s)).collect();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
            if detector().disposed {
                return;
            }
            for (index, source) in WATCHED_SOURCES.iter().enumerate() {
                let current = mtime(*source);
                if current != last[index] {
                    last[index] = current;
                    let internal = current.is_some()
                        && super::get_settings_file_path_for_source(*source).is_some_and(|path| {
                            super::internal_writes::consume_internal_write(&path, 5_000)
                        });
                    if !internal {
                        fan_out(*source);
                    }
                }
            }
        }
    });
}

fn mtime(source: SettingSource) -> Option<SystemTime> {
    super::get_settings_file_path_for_source(source)
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
}

/// Maps to: CC `subscribe` (:173) → `signal.ts` `listeners.add(listener)`.
/// `Set` semantics: re-adding the same identity coalesces into the single
/// existing entry.
pub fn subscribe(listener: SettingsChangeListener) -> SettingsChangeUnsubscribe {
    {
        let mut state = detector();
        if !state
            .listeners
            .iter()
            .any(|entry| Arc::ptr_eq(&entry.listener, &listener))
        {
            let generation = state.next_generation;
            state.next_generation += 1;
            state.listeners.push(ListenerEntry {
                generation,
                listener: Arc::clone(&listener),
            });
        }
    }
    SettingsChangeUnsubscribe { listener }
}

/// Maps to: CC `fanOut` (:437-440) — reset the settings cache (single
/// producer), then emit.
///
/// The emit is a **live** pass, matching `signal.ts`
/// `emit(...args) { for (const listener of listeners) listener(...args) }`:
/// JS iterates the `Set` itself, so a listener that unsubscribes during the
/// pass is genuinely skipped and one added during the pass still runs. A
/// snapshot-then-iterate loop would call listeners that had already
/// unsubscribed — i.e. the RAII cleanup would not actually be a cleanup.
/// Each step re-locks the registry and calls with no lock held.
fn fan_out(source: SettingSource) {
    reset_settings_cache();
    let mut visited: Vec<u64> = Vec::new();
    loop {
        let next = {
            let state = detector();
            state
                .listeners
                .iter()
                .find(|entry| !visited.contains(&entry.generation))
                .map(|entry| (entry.generation, Arc::clone(&entry.listener)))
        };
        let Some((generation, listener)) = next else {
            break;
        };
        visited.push(generation);
        listener(source);
    }
}

/// Maps to: CC `notifyChange` (:447-450) — programmatic notification for
/// settings changes that do not come from the filesystem.
pub fn notify_change(source: SettingSource) {
    crate::utils::debug::log_for_debugging(&format!(
        "Programmatic settings change notification for {source:?}"
    ));
    fan_out(source);
}

/// Maps to: CC `dispose()` (:154-168).
pub fn dispose() {
    let mut state = detector();
    super::internal_writes::clear_internal_writes();
    state.disposed = true;
    state.listeners.clear();
}

/// Maps to: CC `resetForTesting` (:461-480).
#[cfg(test)]
pub fn reset_for_testing() {
    super::internal_writes::clear_internal_writes();
    let mut state = detector();
    state.listeners.clear();
    state.initialized = false;
    state.disposed = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Maps to: CC `fanOut` (:437-440) — the cache reset happens once at the
    /// producer, before listeners run, so the first consumer to read pays the
    /// miss and the rest hit the repopulated cache.
    #[test]
    fn fan_out_resets_cache_before_notifying_listeners() {
        reset_for_testing();
        super::super::settings_cache::set_session_settings_cache(Default::default());

        let saw_empty_cache = Arc::new(AtomicUsize::new(0));
        let saw_empty_cache_in_listener = saw_empty_cache.clone();
        let handle = subscribe(Arc::new(move |_source| {
            if super::super::settings_cache::get_session_settings_cache().is_none() {
                saw_empty_cache_in_listener.fetch_add(1, Ordering::SeqCst);
            }
        }));

        notify_change(SettingSource::User);
        assert_eq!(
            saw_empty_cache.load(Ordering::SeqCst),
            1,
            "listeners must observe an already-reset cache"
        );

        handle.unsubscribe();
        notify_change(SettingSource::User);
        assert_eq!(saw_empty_cache.load(Ordering::SeqCst), 1);
        reset_for_testing();
    }

    /// Maps to: CC `signal.ts` `emit` iterating the live `Set`: a listener
    /// that unsubscribes DURING the pass is skipped, and one subscribed during
    /// the pass runs before the pass ends. A snapshot-then-iterate loop fails
    /// the first half — which is what made the RAII cleanup incomplete.
    #[test]
    fn fan_out_is_a_live_pass_like_the_signal_set() {
        reset_for_testing();
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let late_calls = Arc::new(AtomicUsize::new(0));

        let second: SettingsChangeListener = {
            let order = order.clone();
            let second_calls = second_calls.clone();
            Arc::new(move |_| {
                order.lock().unwrap().push("second");
                second_calls.fetch_add(1, Ordering::SeqCst);
            })
        };
        let late: SettingsChangeListener = {
            let order = order.clone();
            let late_calls = late_calls.clone();
            Arc::new(move |_| {
                order.lock().unwrap().push("late");
                late_calls.fetch_add(1, Ordering::SeqCst);
            })
        };

        // `first` unsubscribes `second` and subscribes `late`, both mid-pass.
        let second_handle = Arc::new(Mutex::new(None::<SettingsChangeUnsubscribe>));
        let first = {
            let order = order.clone();
            let second_handle = second_handle.clone();
            let late = Arc::clone(&late);
            let done = std::sync::atomic::AtomicBool::new(false);
            Arc::new(move |_source| {
                order.lock().unwrap().push("first");
                if !done.swap(true, Ordering::SeqCst) {
                    if let Some(handle) = second_handle.lock().unwrap().as_ref() {
                        handle.unsubscribe();
                    }
                    subscribe(Arc::clone(&late));
                }
            })
        };

        subscribe(first);
        *second_handle.lock().unwrap() = Some(subscribe(second));

        notify_change(SettingSource::User);
        assert_eq!(
            second_calls.load(Ordering::SeqCst),
            0,
            "a listener unsubscribed mid-pass must be skipped, not called from a stale snapshot"
        );
        assert_eq!(
            late_calls.load(Ordering::SeqCst),
            1,
            "a listener subscribed mid-pass runs at the tail of the same pass"
        );
        assert_eq!(&*order.lock().unwrap(), &["first", "late"]);
        reset_for_testing();
    }

    /// Maps to: CC `signal.ts` `listeners.add` — `Set` add coalesces a
    /// duplicate identity into the single existing entry.
    #[test]
    fn duplicate_identity_subscribe_coalesces_like_the_signal_set() {
        reset_for_testing();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_listener = calls.clone();
        let shared: SettingsChangeListener = Arc::new(move |_| {
            calls_in_listener.fetch_add(1, Ordering::SeqCst);
        });

        let first = subscribe(Arc::clone(&shared));
        let _second = subscribe(Arc::clone(&shared));
        notify_change(SettingSource::User);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one Set entry, one call");

        first.unsubscribe(); // either handle removes the single entry
        notify_change(SettingSource::User);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        reset_for_testing();
    }

    /// Maps to: CC `subscribe` returning an unsubscribe closure; repeated
    /// calls are idempotent.
    #[test]
    fn unsubscribe_is_idempotent() {
        reset_for_testing();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_listener = calls.clone();
        let handle = subscribe(Arc::new(move |_| {
            calls_in_listener.fetch_add(1, Ordering::SeqCst);
        }));

        notify_change(SettingSource::Project);
        handle.unsubscribe();
        handle.unsubscribe();
        notify_change(SettingSource::Project);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        reset_for_testing();
    }
}
