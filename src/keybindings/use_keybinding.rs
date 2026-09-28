//! Maps to: CC `keybindings/useKeybinding.ts` — per-component registration
//! of a handler for a keybinding action.
//!
//! CC semantics preserved:
//! - The component's own resolution path handles single-keystroke matches;
//!   chord completions are dispatched by the interceptor's registry.
//! - Missing optional provider context is a no-op, never a private default
//!   runtime.
//! - `is_active` gates both resolution and chord-handler registration.
//! - Registrations are removed on dependency change and component unmount.
//! - A handled event stops propagation; `false` means fall through.

use iocraft::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::keybinding_context::KeybindingRuntime;
use super::matcher::key_event_to_keystroke;
use super::resolver::resolve_key_with_chord_state;
use super::types::{ChordResolveResult, ContextName};

pub type KeybindingHandler = Box<dyn FnMut() -> bool + Send>;
pub type KeybindingHandlers = Vec<(String, KeybindingHandler)>;
type SharedKeybindingHandlers = Arc<Mutex<HashMap<String, KeybindingHandler>>>;

#[derive(Default)]
struct ActiveContextRegistrationHook {
    runtime: Option<KeybindingRuntime>,
    context: Option<ContextName>,
    registered: bool,
}

impl ActiveContextRegistrationHook {
    fn update(
        &mut self,
        runtime: Option<KeybindingRuntime>,
        context: ContextName,
        is_active: bool,
    ) {
        let unchanged = self.registered
            && is_active
            && self.context.as_ref() == Some(&context)
            && self
                .runtime
                .as_ref()
                .zip(runtime.as_ref())
                .is_some_and(|(old, new)| old.same_instance(new));
        if unchanged {
            return;
        }
        self.unregister();
        self.runtime = runtime;
        self.context = Some(context.clone());
        if is_active {
            if let Some(runtime) = self.runtime.as_ref() {
                runtime.register_context(context);
                self.registered = true;
            }
        }
    }

    fn unregister(&mut self) {
        if self.registered {
            if let (Some(runtime), Some(context)) = (&self.runtime, &self.context) {
                runtime.unregister_context(context);
            }
        }
        self.registered = false;
    }
}

impl Drop for ActiveContextRegistrationHook {
    fn drop(&mut self) {
        self.unregister();
    }
}

impl Hook for ActiveContextRegistrationHook {}

struct RegisteredHandler {
    runtime: KeybindingRuntime,
    action: String,
    id: u64,
    context: ContextName,
}

impl Drop for RegisteredHandler {
    fn drop(&mut self) {
        self.runtime.unregister_handler(&self.action, self.id);
    }
}

#[derive(Default)]
struct HandlerRegistrationHook {
    registration: Option<RegisteredHandler>,
}

impl HandlerRegistrationHook {
    fn update(
        &mut self,
        runtime: Option<KeybindingRuntime>,
        action: &str,
        context: ContextName,
        is_active: bool,
        handler: impl Fn() + Send + Sync + 'static,
    ) {
        let unchanged = is_active
            && self.registration.as_ref().is_some_and(|registration| {
                registration.action == action
                    && registration.context == context
                    && runtime
                        .as_ref()
                        .is_some_and(|runtime| registration.runtime.same_instance(runtime))
            });
        if unchanged {
            return;
        }
        self.registration = None;
        let Some(runtime) = runtime.filter(|_| is_active) else {
            return;
        };
        let id = runtime.register_handler(action, context.clone(), handler);
        self.registration = Some(RegisteredHandler {
            runtime,
            action: action.to_string(),
            id,
            context,
        });
    }
}

impl Hook for HandlerRegistrationHook {}

#[derive(Default)]
struct HandlerSetRegistrationHook {
    registrations: Vec<RegisteredHandler>,
    runtime: Option<KeybindingRuntime>,
    context: Option<ContextName>,
    actions: Vec<String>,
    active: bool,
}

impl HandlerSetRegistrationHook {
    fn update(
        &mut self,
        runtime: Option<KeybindingRuntime>,
        context: ContextName,
        actions: Vec<String>,
        is_active: bool,
        handlers: SharedKeybindingHandlers,
    ) {
        let unchanged = self.active == is_active
            && self.context.as_ref() == Some(&context)
            && self.actions == actions
            && self
                .runtime
                .as_ref()
                .zip(runtime.as_ref())
                .is_some_and(|(old, new)| old.same_instance(new));
        if unchanged {
            return;
        }

        self.registrations.clear();
        self.runtime = runtime;
        self.context = Some(context.clone());
        self.actions = actions.clone();
        self.active = is_active;
        let Some(runtime) = self.runtime.as_ref().filter(|_| is_active) else {
            return;
        };
        for action in actions {
            let action_for_callback = action.clone();
            let handlers = Arc::clone(&handlers);
            let id = runtime.register_handler(&action, context.clone(), move || {
                if let Some(handler) = handlers
                    .lock()
                    .expect("keybinding handlers lock")
                    .get_mut(&action_for_callback)
                {
                    let _ = handler();
                }
            });
            self.registrations.push(RegisteredHandler {
                runtime: runtime.clone(),
                action,
                id,
                context: context.clone(),
            });
        }
    }
}

impl Hook for HandlerSetRegistrationHook {}

/// Maps to: CC `useRegisterKeybindingContext(context, isActive)` including
/// layout-effect cleanup on dependency change and component unmount.
pub fn use_register_keybinding_context(
    hooks: &mut Hooks,
    runtime: Option<KeybindingRuntime>,
    context: ContextName,
    is_active: bool,
) {
    hooks
        .use_hook(ActiveContextRegistrationHook::default)
        .update(runtime, context, is_active);
}

/// Maps to: CC `useKeybinding(action, handler, options)`.
///
/// The optional runtime mirrors `useOptionalKeybindingContext()`. Production
/// components rely on the root provider; tests that need bindings mount that
/// provider instead of manufacturing component-local defaults.
pub fn use_keybinding<IsActive, Handler>(
    hooks: &mut Hooks,
    runtime: Option<KeybindingRuntime>,
    action: &'static str,
    context: ContextName,
    is_active: IsActive,
    handler: Handler,
) where
    IsActive: Fn() -> bool + Send + 'static,
    Handler: FnMut() -> bool + Send + 'static,
{
    type SharedHandler = Arc<Mutex<Box<dyn FnMut() -> bool + Send>>>;
    let shared: SharedHandler = hooks.use_const(|| {
        Arc::new(Mutex::new(
            Box::new(|| false) as Box<dyn FnMut() -> bool + Send>
        ))
    });
    *shared.lock().expect("keybinding handler lock") = Box::new(handler);
    let active_check = Arc::new(Mutex::new(is_active));
    let active = active_check.lock().expect("keybinding active check lock")();

    {
        let shared_for_chord = Arc::clone(&shared);
        hooks.use_hook(HandlerRegistrationHook::default).update(
            runtime.clone(),
            action,
            context.clone(),
            active,
            move || {
                let _ = (*shared_for_chord.lock().expect("keybinding handler lock"))();
            },
        );
    }

    hooks.use_propagated_terminal_events(move |event| {
        if event.is_propagation_stopped()
            || !active_check.lock().expect("keybinding active check lock")()
        {
            return;
        }
        let Some(runtime) = runtime.as_ref() else {
            return;
        };
        let TerminalEvent::Key(key_event) = event.event() else {
            return;
        };
        // During chord wait the setup interceptor owns dispatch.
        if runtime.chord_pending() {
            return;
        }
        let Some(keystroke) = key_event_to_keystroke(key_event) else {
            return;
        };
        let mut contexts: HashSet<ContextName> = runtime.active_contexts();
        contexts.insert(context.clone());
        contexts.insert(ContextName::Global);

        let result = resolve_key_with_chord_state(
            Some(&keystroke),
            keystroke.key == "escape",
            &contexts,
            runtime.bindings().as_slice(),
            None,
        );
        match result {
            ChordResolveResult::Match { action: resolved } if resolved == action => {
                let handled = runtime.dispatch(vec![keystroke.clone()], || {
                    (*shared.lock().expect("keybinding handler lock"))()
                });
                if handled {
                    event.stop_propagation();
                }
            }
            ChordResolveResult::Unbound => event.stop_propagation(),
            _ => {}
        }
    });
}

/// Maps to: CC `useKeybindings(handlers, options)` including dynamic action
/// sets such as user-configured `command:*` bindings.
pub fn use_keybindings<IsActive>(
    hooks: &mut Hooks,
    runtime: Option<KeybindingRuntime>,
    handlers: KeybindingHandlers,
    context: ContextName,
    is_active: IsActive,
) where
    IsActive: Fn() -> bool + Send + 'static,
{
    let shared: SharedKeybindingHandlers = hooks.use_const(|| Arc::new(Mutex::new(HashMap::new())));
    let mut action_order = Vec::new();
    let mut next_handlers = HashMap::new();
    for (action, handler) in handlers {
        if !next_handlers.contains_key(&action) {
            action_order.push(action.clone());
        }
        next_handlers.insert(action, handler);
    }
    *shared.lock().expect("keybinding handlers lock") = next_handlers;
    let active_check = Arc::new(Mutex::new(is_active));
    let active = active_check.lock().expect("keybinding active check lock")();

    hooks.use_hook(HandlerSetRegistrationHook::default).update(
        runtime.clone(),
        context.clone(),
        action_order,
        active,
        Arc::clone(&shared),
    );

    hooks.use_propagated_terminal_events(move |event| {
        if event.is_propagation_stopped()
            || !active_check.lock().expect("keybinding active check lock")()
        {
            return;
        }
        let Some(runtime) = runtime.as_ref() else {
            return;
        };
        let TerminalEvent::Key(key_event) = event.event() else {
            return;
        };
        if runtime.chord_pending() {
            return;
        }
        let Some(keystroke) = key_event_to_keystroke(key_event) else {
            return;
        };
        let mut contexts: HashSet<ContextName> = runtime.active_contexts();
        contexts.insert(context.clone());
        contexts.insert(ContextName::Global);
        match resolve_key_with_chord_state(
            Some(&keystroke),
            keystroke.key == "escape",
            &contexts,
            runtime.bindings().as_slice(),
            None,
        ) {
            ChordResolveResult::Match { action } => {
                if let Some(handler) = shared
                    .lock()
                    .expect("keybinding handlers lock")
                    .get_mut(&action)
                {
                    if runtime.dispatch(vec![keystroke.clone()], || handler()) {
                        event.stop_propagation();
                    }
                }
            }
            ChordResolveResult::Unbound => event.stop_propagation(),
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::time::Duration;

    #[component]
    fn MissingProviderHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut fired = hooks.use_state(|| false);
        use_keybinding(
            &mut hooks,
            None,
            "chat:submit",
            ContextName::Chat,
            || true,
            move || {
                fired.set(true);
                true
            },
        );
        element! { Text(content: if fired.get() { "fired" } else { "waiting" }.to_string()) }
    }

    #[component]
    fn MultiHandlerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = hooks.use_const(|| {
            KeybindingRuntime::new(vec![crate::keybindings::types::ParsedBinding {
                chord: crate::keybindings::parser::parse_chord("enter"),
                action: Some("command:test".to_string()),
                context: ContextName::Chat,
            }])
        });
        let mut fired = hooks.use_state(|| false);
        use_keybindings(
            &mut hooks,
            Some(runtime),
            vec![(
                "command:test".to_string(),
                Box::new(move || {
                    fired.set(true);
                    true
                }),
            )],
            ContextName::Chat,
            || true,
        );
        element! { Text(content: if fired.get() { "multi-fired" } else { "waiting" }.to_string()) }
    }

    #[component]
    fn ChordHandlerHarness(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings(),
        );
        let mut fired = hooks.use_state(|| false);
        use_keybinding(
            &mut hooks,
            Some(runtime.clone()),
            "chat:externalEditor",
            ContextName::Chat,
            || true,
            move || {
                fired.set(true);
                true
            },
        );
        element! {
            ContextProvider(value: Context::owned(runtime)) {

                Text(content: if fired.get() { "fired" } else { "waiting" }.to_string())
            }
        }
    }
    use crate::keybindings::parser::parse_keystroke;
    use crate::keybindings::resolver::resolve_key_with_chord_state as resolve;

    #[test]
    fn optional_provider_is_a_no_op_instead_of_creating_private_defaults() {
        let text = futures::executor::block_on(async move {
            let mut app = element!(MissingProviderHarness);
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![TerminalEvent::Key(
                        KeyEvent::new(KeyEventKind::Press, KeyCode::Enter),
                    )]))
                    .with_size(20, 4),
                ),
            );
            let mut last = String::new();
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
            }
            last
        });
        assert!(text.contains("waiting"), "canvas=\n{text}");
    }

    #[test]
    fn use_keybindings_dispatches_dynamic_action_maps() {
        // mock_terminal_render_loop does not end when the event stream ends;
        // race each frame against a short delay (same as sibling tests).
        let text = futures::executor::block_on(async move {
            let mut app = element!(MultiHandlerHarness);
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![TerminalEvent::Key(
                        KeyEvent::new(KeyEventKind::Press, KeyCode::Enter),
                    )]))
                    .with_size(20, 4),
                ),
            );
            let mut last = String::new();
            for _ in 0..16 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if last.contains("multi-fired") {
                    break;
                }
            }
            last
        });
        assert!(text.contains("multi-fired"), "canvas=\n{text}");
    }

    #[test]
    fn use_keybinding_registers_live_chord_completion_handler() {
        let mut ctrl_x = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'));
        ctrl_x.modifiers = KeyModifiers::CONTROL;
        let mut ctrl_e = KeyEvent::new(KeyEventKind::Press, KeyCode::Char('e'));
        ctrl_e.modifiers = KeyModifiers::CONTROL;
        let text = futures::executor::block_on(async move {
            let mut app = element!(ChordHandlerHarness);
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        TerminalEvent::Key(ctrl_x),
                        TerminalEvent::Key(ctrl_e),
                    ]))
                    .with_size(20, 4),
                ),
            );
            let mut last = None;
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else { break };
                last = Some(canvas.to_string());
            }
            last.unwrap_or_default()
        });
        assert!(text.contains("fired"), "canvas=\n{text}");
    }

    /// The hook body is exercised through iocraft's event loop in component
    /// tests; the pure gating logic it composes is covered here.
    #[test]
    fn active_context_registration_unregisters_on_drop() {
        let runtime = KeybindingRuntime::with_default_bindings();
        let mut registration = ActiveContextRegistrationHook::default();
        registration.update(Some(runtime.clone()), ContextName::Help, true);
        assert!(runtime.active_contexts().contains(&ContextName::Help));
        drop(registration);
        assert!(!runtime.active_contexts().contains(&ContextName::Help));
    }

    #[test]
    fn chord_handler_registration_unregisters_on_drop() {
        let runtime = KeybindingRuntime::with_default_bindings();
        let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fired_in_handler = Arc::clone(&fired);
        let mut registration = HandlerRegistrationHook::default();
        registration.update(
            Some(runtime.clone()),
            "chat:killAgents",
            ContextName::Chat,
            true,
            move || {
                fired_in_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        );
        drop(registration);
        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        runtime.observe_keystroke(&parse_keystroke("ctrl+k"));
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn multi_handler_registration_replaces_actions_and_cleans_up_on_drop() {
        let runtime = KeybindingRuntime::with_default_bindings();
        let kill_fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let editor_fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handlers: SharedKeybindingHandlers = Arc::new(Mutex::new(HashMap::from([
            ("chat:killAgents".to_string(), {
                let fired = Arc::clone(&kill_fired);
                Box::new(move || {
                    fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    true
                }) as KeybindingHandler
            }),
            ("chat:externalEditor".to_string(), {
                let fired = Arc::clone(&editor_fired);
                Box::new(move || {
                    fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    true
                }) as KeybindingHandler
            }),
        ])));
        let mut registration = HandlerSetRegistrationHook::default();
        registration.update(
            Some(runtime.clone()),
            ContextName::Chat,
            vec!["chat:killAgents".to_string()],
            true,
            Arc::clone(&handlers),
        );
        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        runtime.observe_keystroke(&parse_keystroke("ctrl+k"));
        assert_eq!(kill_fired.load(std::sync::atomic::Ordering::SeqCst), 1);

        registration.update(
            Some(runtime.clone()),
            ContextName::Chat,
            vec!["chat:externalEditor".to_string()],
            true,
            Arc::clone(&handlers),
        );
        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        runtime.observe_keystroke(&parse_keystroke("ctrl+k"));
        assert_eq!(kill_fired.load(std::sync::atomic::Ordering::SeqCst), 1);
        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        runtime.observe_keystroke(&parse_keystroke("ctrl+e"));
        assert_eq!(editor_fired.load(std::sync::atomic::Ordering::SeqCst), 1);

        drop(registration);
        runtime.observe_keystroke(&parse_keystroke("ctrl+x"));
        runtime.observe_keystroke(&parse_keystroke("ctrl+e"));
        assert_eq!(editor_fired.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn component_context_joins_runtime_contexts_for_resolution() {
        let runtime = KeybindingRuntime::with_default_bindings();
        // Help is not in the resting contexts…
        assert!(!runtime.active_contexts().contains(&ContextName::Help));
        // …but a use_keybinding site in Help context resolves help:dismiss.
        let mut contexts = runtime.active_contexts();
        contexts.insert(ContextName::Help);
        contexts.insert(ContextName::Global);
        let result = resolve(
            Some(&parse_keystroke("escape")),
            true,
            &contexts,
            runtime.bindings().as_slice(),
            None,
        );
        // Last-wins across Chat+Help: both bind escape; Help's binding is
        // later in the table, so the deeper component's action wins.
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "help:dismiss".into()
            }
        );
    }

    /// Last-wins: when Autocomplete is active alongside Chat, ↑ resolves to
    /// autocomplete navigation rather than history:previous.
    #[test]
    fn autocomplete_context_wins_up_over_chat_history() {
        let runtime = KeybindingRuntime::with_default_bindings();
        let mut contexts = runtime.active_contexts();
        contexts.insert(ContextName::Chat);
        contexts.insert(ContextName::Autocomplete);
        contexts.insert(ContextName::Global);
        let result = resolve(
            Some(&parse_keystroke("up")),
            false,
            &contexts,
            runtime.bindings().as_slice(),
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "autocomplete:previous".into()
            }
        );
    }

    /// Without Autocomplete registered, Chat still owns ↑ for history.
    #[test]
    fn chat_history_owns_up_without_autocomplete_context() {
        let runtime = KeybindingRuntime::with_default_bindings();
        let mut contexts = runtime.active_contexts();
        contexts.insert(ContextName::Chat);
        contexts.insert(ContextName::Global);
        let result = resolve(
            Some(&parse_keystroke("up")),
            false,
            &contexts,
            runtime.bindings().as_slice(),
            None,
        );
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: "history:previous".into()
            }
        );
    }
}
