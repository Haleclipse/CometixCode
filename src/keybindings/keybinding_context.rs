//! Maps to: CC `keybindings/KeybindingContext.tsx` — keybinding runtime
//! state, pending chord, handler registry, and active contexts.
//!

//! Chord timeout is lazy: CC clears pending via a 1s setTimeout; here the
//! deadline is checked when the next key arrives. Behaviorally equivalent
//! (pending only matters at the next keystroke) and timer-free; there is no
//! chord-wait UI either way.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use super::default_bindings::default_bindings;
use super::keybinding_provider_setup::CHORD_TIMEOUT;
use super::resolver::resolve_key_with_chord_state;
use super::types::{ChordResolveResult, ContextName, ParsedBinding, ParsedKeystroke};

/// Maps to: CC `HandlerRegistration` in KeybindingContext.tsx — a handler
/// bound to an action, gated by its registration context being active.
type ActionHandler = Arc<dyn Fn() + Send + Sync>;

struct HandlerRegistration {
    id: u64,
    context: ContextName,
    handler: ActionHandler,
}

struct KeybindingRuntimeInner {
    bindings: RwLock<Arc<Vec<ParsedBinding>>>,
    /// Maps to: CC `pendingChordRef` (+ deadline for the lazy timeout).
    pending: Mutex<Option<(Vec<ParsedKeystroke>, Instant)>>,
    /// Maps to: CC handler registry `Map<string, Set<HandlerRegistration>>`.
    /// Serves chord-completion dispatch (CC ChordInterceptor invokeAction);
    /// single-key matches are handled by each `use_keybinding` site.
    handlers: Mutex<HashMap<String, Vec<HandlerRegistration>>>,
    next_handler_id: AtomicU64,
    /// Maps to: CC `activeContexts: Set<KeybindingContextName>`.
    active_contexts: Mutex<HashSet<ContextName>>,
    /// The keys whose action is being handled, set around each handler call.
    /// Cometix-specific deviation (product requirement — skip in parity
    /// audits): CC handlers learn only the action; the exit hook names the
    /// key that was pressed, since Ctrl+C is bound to `app:exit` here.
    dispatching: Mutex<Option<Vec<ParsedKeystroke>>>,
}

/// Cheap-to-clone runtime handle injected via ContextProvider.
/// Maps to: CC `KeybindingContextValue`.
#[derive(Clone)]
pub struct KeybindingRuntime {
    inner: Arc<KeybindingRuntimeInner>,
}

impl KeybindingRuntime {
    pub fn new(bindings: Vec<ParsedBinding>) -> Self {
        Self {
            inner: Arc::new(KeybindingRuntimeInner {
                bindings: RwLock::new(Arc::new(bindings)),
                pending: Mutex::new(None),
                handlers: Mutex::new(HashMap::new()),
                next_handler_id: AtomicU64::new(1),
                active_contexts: Mutex::new(HashSet::new()),
                dispatching: Mutex::new(None),
            }),
        }
    }

    /// Runs `handler` with `keys` recorded as the keys being handled (see
    /// `dispatching`), restoring the previous record afterwards.
    pub fn dispatch<R>(&self, keys: Vec<ParsedKeystroke>, handler: impl FnOnce() -> R) -> R {
        let previous = self
            .inner
            .dispatching
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(keys);
        let result = handler();
        *self
            .inner
            .dispatching
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = previous;
        result
    }

    /// The keys whose action is being handled, while a handler runs.
    pub fn dispatching_keys(&self) -> Option<Vec<ParsedKeystroke>> {
        self.inner
            .dispatching
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn with_default_bindings() -> Self {
        Self::new(default_bindings())
    }

    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn bindings(&self) -> Arc<Vec<ParsedBinding>> {
        self.inner
            .bindings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Maps to keybinding-change subscription updating provider state.
    pub fn replace_bindings(&self, bindings: Vec<ParsedBinding>) {
        *self
            .inner
            .bindings
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(bindings);
        self.set_pending(None);
    }

    pub fn active_contexts(&self) -> HashSet<ContextName> {
        self.inner
            .active_contexts
            .lock()
            .expect("contexts lock")
            .clone()
    }

    /// Maps to: CC `useRegisterKeybindingContext(context, isActive)` mount.
    pub fn register_context(&self, context: ContextName) {
        self.inner
            .active_contexts
            .lock()
            .expect("contexts lock")
            .insert(context);
    }

    pub fn unregister_context(&self, context: &ContextName) {
        self.inner
            .active_contexts
            .lock()
            .expect("contexts lock")
            .remove(context);
    }

    /// Maps to: CC `registerHandler`; the returned id is owned by the hook and
    /// removed on dependency change or component unmount.
    pub fn register_handler(
        &self,
        action: &str,
        context: ContextName,
        handler: impl Fn() + Send + Sync + 'static,
    ) -> u64 {
        let id = self.inner.next_handler_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .handlers
            .lock()
            .expect("handlers lock")
            .entry(action.to_string())
            .or_default()
            .push(HandlerRegistration {
                id,
                context,
                handler: Arc::new(handler),
            });
        id
    }

    pub fn unregister_handler(&self, action: &str, id: u64) {
        let mut handlers = self.inner.handlers.lock().expect("handlers lock");
        let mut remove_action = false;
        if let Some(entries) = handlers.get_mut(action) {
            entries.retain(|registration| registration.id != id);
            remove_action = entries.is_empty();
        }
        if remove_action {
            handlers.remove(action);
        }
    }

    /// Whether an action currently has a mounted handler in one of the
    /// contexts participating in resolution.
    pub fn has_registered_handler(&self, action: &str, contexts: &HashSet<ContextName>) -> bool {
        self.inner
            .handlers
            .lock()
            .expect("handlers lock")
            .get(action)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|registration| contexts.contains(&registration.context))
            })
    }

    /// True while a chord prefix is buffered — `use_keybinding` sites and
    /// cooperative input layers skip single-key handling during chord wait.
    pub fn chord_pending(&self) -> bool {
        self.current_pending().is_some()
    }

    fn current_pending(&self) -> Option<Vec<ParsedKeystroke>> {
        let mut slot = self.inner.pending.lock().expect("pending lock");
        match &*slot {
            Some((keys, deadline)) if deadline.elapsed() < CHORD_TIMEOUT => Some(keys.clone()),
            Some(_) => {
                // Lazy timeout expiry (CC: setTimeout clears the ref).
                *slot = None;
                None
            }
            None => None,
        }
    }

    fn set_pending(&self, keys: Option<Vec<ParsedKeystroke>>) {
        *self.inner.pending.lock().expect("pending lock") = keys.map(|k| (k, Instant::now()));
    }

    fn interceptor_contexts(&self) -> HashSet<ContextName> {
        let mut contexts = self.active_contexts();
        contexts.insert(ContextName::Global);
        for entries in self.inner.handlers.lock().expect("handlers lock").values() {
            contexts.extend(
                entries
                    .iter()
                    .map(|registration| registration.context.clone()),
            );
        }
        contexts
    }

    /// Maps to: CC `ChordInterceptor` selecting the first registered handler
    /// whose context participated in resolution.
    fn invoke_action(
        &self,
        action: &str,
        contexts: &HashSet<ContextName>,
        chord: Vec<ParsedKeystroke>,
    ) {
        let handler = self
            .inner
            .handlers
            .lock()
            .expect("handlers lock")
            .get(action)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|registration| contexts.contains(&registration.context))
                    .map(|registration| Arc::clone(&registration.handler))
            });
        if let Some(handler) = handler {
            self.dispatch(chord, || handler());
        }
    }

    /// Maps to: CC ChordInterceptor's per-keystroke logic
    /// (KeybindingProviderSetup.tsx). Returns the resolution for observers.
    pub fn observe_keystroke(&self, keystroke: &ParsedKeystroke) -> ChordResolveResult {
        let pending = self.current_pending();
        let bindings = self.bindings();
        let contexts = self.interceptor_contexts();
        let result = resolve_key_with_chord_state(
            Some(keystroke),
            keystroke.key == "escape",
            &contexts,
            bindings.as_slice(),
            pending.as_deref(),
        );
        match &result {
            ChordResolveResult::ChordStarted { pending } => {
                self.set_pending(Some(pending.clone()));
            }
            ChordResolveResult::Match { action } => {
                self.set_pending(None);
                // CC: only chord completions dispatch through the registry;
                // single-key matches belong to per-component use_keybinding.
                if let Some(mut chord) = pending {
                    chord.push(keystroke.clone());
                    self.invoke_action(action, &contexts, chord);
                }
            }
            ChordResolveResult::ChordCancelled | ChordResolveResult::Unbound => {
                self.set_pending(None);
            }
            ChordResolveResult::None => {
                self.set_pending(None);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::parser::parse_keystroke;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn chord_completion_dispatches_registered_handler() {
        let runtime = KeybindingRuntime::with_default_bindings();
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_in_handler = fired.clone();
        runtime.register_handler("chat:killAgents", ContextName::Chat, move || {
            fired_in_handler.fetch_add(1, Ordering::SeqCst);
        });

        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        assert!(runtime.chord_pending());
        runtime.observe_keystroke(&parse_keystroke("ctrl+k"));
        assert!(!runtime.chord_pending());
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn single_key_match_does_not_dispatch_through_registry() {
        // CC: single-key matches are the per-component hooks' business.
        let runtime = KeybindingRuntime::with_default_bindings();
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_in_handler = fired.clone();
        runtime.register_handler("chat:submit", ContextName::Chat, move || {
            fired_in_handler.fetch_add(1, Ordering::SeqCst);
        });

        runtime.observe_keystroke(&parse_keystroke("enter"));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn escape_cancels_chord_wait() {
        let runtime = KeybindingRuntime::with_default_bindings();
        runtime.register_context(ContextName::Chat);
        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        assert!(runtime.chord_pending());
        let result = runtime.observe_keystroke(&parse_keystroke("escape"));
        assert_eq!(result, ChordResolveResult::ChordCancelled);
        assert!(!runtime.chord_pending());
    }

    #[test]
    fn chord_pending_expires_lazily_after_timeout() {
        let runtime = KeybindingRuntime::with_default_bindings();
        runtime.register_context(ContextName::Chat);
        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        assert!(runtime.chord_pending());
        // Simulate expiry by rewinding the stored instant.
        {
            let mut slot = runtime.inner.pending.lock().unwrap();
            if let Some((_, deadline)) = slot.as_mut() {
                *deadline = Instant::now() - CHORD_TIMEOUT - Duration::from_millis(1);
            }
        }
        assert!(!runtime.chord_pending());
        // Next ctrl+k resolves standalone (no stale chord completion) —
        // ctrl+k alone is an editing key, unbound in the tables.
        let result = runtime.observe_keystroke(&parse_keystroke("ctrl+k"));
        assert_eq!(result, ChordResolveResult::None);
    }

    #[test]
    fn replacing_bindings_after_editor_reload_updates_live_resolution() {
        let runtime = KeybindingRuntime::with_default_bindings();
        runtime.register_context(ContextName::Chat);
        let mut updated = runtime.bindings().as_ref().clone();
        updated.push(ParsedBinding {
            chord: crate::keybindings::parser::parse_chord("enter"),
            action: Some("chat:cancel".to_string()),
            context: ContextName::Chat,
        });
        runtime.replace_bindings(updated);

        assert_eq!(
            runtime.observe_keystroke(&parse_keystroke("enter")),
            ChordResolveResult::Match {
                action: "chat:cancel".to_string()
            }
        );
    }

    #[test]
    fn unregister_handler_removes_chord_callback() {
        let runtime = KeybindingRuntime::with_default_bindings();
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_in_handler = fired.clone();
        let id = runtime.register_handler("chat:killAgents", ContextName::Chat, move || {
            fired_in_handler.fetch_add(1, Ordering::SeqCst);
        });
        runtime.unregister_handler("chat:killAgents", id);
        runtime.register_context(ContextName::Chat);
        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        runtime.observe_keystroke(&parse_keystroke("ctrl+k"));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }
}
