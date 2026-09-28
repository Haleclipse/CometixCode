//! Maps to: CC `context/overlayContext.tsx` — overlay tracking for Escape
//! key coordination, backed by `AppState.activeOverlays`.
//!
//! CancelRequestHandler (keybindings `chat:cancel`) must not abort a running
//! query when the user is merely dismissing an open overlay (Select with
//! onCancel, FuzzyPicker, ...). Overlay components register themselves while
//! mounted; the cancel handler checks `is_overlay_active` in its isActive
//! gate.
//!
//! CC registers/unregisters via useEffect mount/cleanup. iocraft components
//! have no unmount cleanup hook exposed to user code, so registration here
//! is guard-based: `OverlayRegistration` unregisters on Drop, and callers
//! hold it in `use_state` so its lifetime matches the component's.
//!
//! The guards of one id are counted, and the id leaves the Set with the
//! last one. React runs a commit's unmount cleanups before its mount
//! effects, so when one dialog replaces another (both with a cancellable
//! Select) CC removes 'select' and adds it again; iocraft mounts the new
//! child before it drops the old one, and a plain Set would lose the id to
//! the old guard's Drop. Counting gives CC's end state in that order.

use std::sync::{Arc, LazyLock, Mutex};

use crate::state::store::{AppStore, UpdateDecision};

/// Live guards per (store, id); an entry goes when its count reaches zero,
/// so it never outlives the guards that hold the same store.
static LIVE_GUARDS: LazyLock<Mutex<Vec<(AppStore, Arc<str>, usize)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Maps to: CC `NON_MODAL_OVERLAYS` — overlays that shouldn't disable
/// TextInput focus.
const NON_MODAL_OVERLAYS: &[&str] = &["autocomplete"];

/// RAII registration: the overlay id stays in `AppState.active_overlays`
/// while this guard lives. Maps to: CC `useRegisterOverlay(id, enabled)`
/// effect registration + cleanup.
pub struct OverlayRegistration {
    store: AppStore,
    id: Arc<str>,
}

impl OverlayRegistration {
    pub fn register(store: AppStore, id: &str) -> Self {
        let id: Arc<str> = id.into();
        {
            let mut live = LIVE_GUARDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match live
                .iter_mut()
                .find(|(other, other_id, _)| other.same_instance(&store) && *other_id == id)
            {
                Some((_, _, count)) => *count += 1,
                None => live.push((store.clone(), id.clone(), 1)),
            }
        }
        // P3 §3a: an already-registered id returns `prev` untouched (CC
        // effect no-ops when the Set already holds the id).
        store.set_state(|prev| {
            if prev.active_overlays.contains(id.as_ref()) {
                return UpdateDecision::Same(());
            }
            let mut overlays = (*prev.active_overlays).clone();
            overlays.insert(id.to_string());
            let mut next = (**prev).clone();
            next.active_overlays = Arc::new(overlays);
            UpdateDecision::Replace {
                next: Arc::new(next),
                result: (),
            }
        });
        Self { store, id }
    }
}

impl Drop for OverlayRegistration {
    fn drop(&mut self) {
        // Only the last live guard of the id removes it (module doc).
        let last = {
            let mut live = LIVE_GUARDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match live.iter().position(|(other, other_id, _)| {
                other.same_instance(&self.store) && *other_id == self.id
            }) {
                Some(index) => {
                    live[index].2 -= 1;
                    if live[index].2 == 0 {
                        live.remove(index);
                        true
                    } else {
                        false
                    }
                }
                None => true,
            }
        };
        if !last {
            return;
        }
        // P3 §3a: dropping a guard whose id is already gone returns `prev`
        // untouched — Same also keeps this Drop from re-entering the effect
        // pipeline needlessly.
        self.store.set_state(|prev| {
            if !prev.active_overlays.contains(self.id.as_ref()) {
                return UpdateDecision::Same(());
            }
            let mut overlays = (*prev.active_overlays).clone();
            overlays.remove(self.id.as_ref());
            let mut next = (**prev).clone();
            next.active_overlays = Arc::new(overlays);
            UpdateDecision::Replace {
                next: Arc::new(next),
                result: (),
            }
        });
    }
}

/// Maps to: CC `overlayContext.tsx:87-89 useIsOverlayActive()` —
/// `useAppState(s => s.activeOverlays.size > 0)`, i.e. a SUBSCRIBED render-time
/// read. Use this from a component body; [`is_overlay_active`] is the live
/// store read for imperative call sites (keybinding `is_active` closures,
/// evaluated per keystroke rather than per render).
pub fn use_is_overlay_active(hooks: &mut iocraft::prelude::Hooks) -> bool {
    crate::state::app_state::use_app_state(hooks, |state| !state.active_overlays.is_empty())
}

/// Maps to: CC `overlayContext.tsx:102-109 useIsModalOverlayActive()` — the
/// subscribed read of "any overlay that should capture all input" (non-modal
/// overlays like autocomplete don't disable TextInput).
pub fn use_is_modal_overlay_active(hooks: &mut iocraft::prelude::Hooks) -> bool {
    crate::state::app_state::use_app_state(hooks, modal_overlay_active)
}

/// Live store read behind [`use_is_overlay_active`], for imperative callers.
pub fn is_overlay_active(store: &AppStore) -> bool {
    !store.get().active_overlays.is_empty()
}

/// Live store read behind [`use_is_modal_overlay_active`], for imperative
/// callers.
pub fn is_modal_overlay_active(store: &AppStore) -> bool {
    modal_overlay_active(&store.get())
}

fn modal_overlay_active(state: &crate::state::app_state_store::AppState) -> bool {
    state
        .active_overlays
        .iter()
        .any(|id| !NON_MODAL_OVERLAYS.contains(&id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;
    use iocraft::prelude::*;

    #[test]
    fn registration_guard_adds_and_removes_overlay_like_cc_effect_cleanup() {
        let store = AppStore::new(AppState::default(), None);
        assert!(!is_overlay_active(&store));

        {
            let _guard = OverlayRegistration::register(store.clone(), "select");
            assert!(is_overlay_active(&store));
            assert!(is_modal_overlay_active(&store));
        }
        assert!(!is_overlay_active(&store));
    }

    #[test]
    fn autocomplete_is_non_modal_like_cc_allowlist() {
        let store = AppStore::new(AppState::default(), None);
        let _guard = OverlayRegistration::register(store.clone(), "autocomplete");
        assert!(is_overlay_active(&store));
        assert!(!is_modal_overlay_active(&store));
    }

    #[test]
    fn a_replacement_registered_before_the_old_guard_drops_keeps_the_overlay() {
        // iocraft mounts the new dialog's Select before it drops the old
        // one; CC's commit order (cleanup, then mount) ends with 'select'
        // registered, and so does this.
        let store = AppStore::new(AppState::default(), None);
        let old = OverlayRegistration::register(store.clone(), "select");
        let new = OverlayRegistration::register(store.clone(), "select");
        drop(old);
        assert!(is_overlay_active(&store));
        drop(new);
        assert!(!is_overlay_active(&store));
        // Counts are per store.
        let other = AppStore::new(AppState::default(), None);
        let guard = OverlayRegistration::register(other.clone(), "select");
        assert!(!is_overlay_active(&store));
        drop(guard);
        assert!(!is_overlay_active(&other));
    }

    #[derive(Default, Props)]
    struct HolderProps {
        store: Option<AppStore>,
    }

    /// Holds a 'select' registration for as long as it is mounted, as a
    /// cancellable Select does.
    #[component]
    fn Holder(props: &HolderProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let store = props.store.clone();
        let _registration =
            hooks.use_state(move || store.map(|store| OverlayRegistration::register(store, "select")));
        element!(View)
    }

    #[derive(Default, Props)]
    struct SwapperProps {
        store: Option<AppStore>,
    }

    /// Each key replaces the keyed Holder with a new one, as the next queued
    /// permission dialog replaces the answered one.
    #[component]
    fn Swapper(props: &SwapperProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut generation = hooks.use_state(|| 0u32);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                generation.set(generation.get() + 1);
            }
        });
        element! {
            View {
                Holder(key: generation.get(), store: props.store.clone())
                Text(content: format!("generation={}", generation.get()))
            }
        }
    }

    #[test]
    fn a_keyed_replacement_keeps_the_overlay_through_the_swap() {
        let store = AppStore::new(AppState::default(), None);
        let (settled, active) = futures::executor::block_on({
            let store = store.clone();
            async move {
                let probe = store.clone();
                use futures::StreamExt;
                let mut app = element!(Swapper(store: Some(store)));
                let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(futures::stream::iter(vec![TerminalEvent::Key(
                        KeyEvent::new(KeyEventKind::Press, KeyCode::F(12)),
                    )]))
                    .with_size(40, 5),
                ));
                loop {
                    let next = crate::utils::race(render_loop.next(), async {
                        futures_timer::Delay::new(std::time::Duration::from_secs(10)).await;
                        None
                    })
                    .await;
                    let canvas = next.expect("a frame after the swap").to_string();
                    if canvas.contains("generation=1") {
                        // Read while the tree is still mounted.
                        break (canvas, is_overlay_active(&probe));
                    }
                }
            }
        });
        assert!(settled.contains("generation=1"), "{settled}");
        // The old Holder is gone by the time generation 1 is on screen; its
        // Drop ran after the new Holder registered.
        assert!(active);
        // Unmounting the tree unregisters the last guard.
        assert!(!is_overlay_active(&store));
    }
}
