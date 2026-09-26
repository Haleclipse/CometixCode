//! Maps to: CC `keybindings/KeybindingProviderSetup.tsx`.
//!
//! iocraft adaptation: the interceptor is a bypass terminal-event listener.
//! It observes every key while per-component hooks retain ownership of
//! single-keystroke propagation. The startup load fills the loader cache and
//! the watcher runs off the render thread; each `KeybindingSetup` mount reads
//! the cache and subscribes to reloads.

use iocraft::prelude::*;
use std::time::Duration;

use super::keybinding_context::KeybindingRuntime;
use super::matcher::key_event_to_keystroke;

/// Maps to: CC `KeybindingProviderSetup.tsx` `CHORD_TIMEOUT_MS`.
pub(super) const CHORD_TIMEOUT: Duration = Duration::from_millis(1000);
const WARNING_NOTIFICATION_KEY: &str = "keybinding-config-warning";

/// Maps to: CC `useKeybindingWarnings()` notification projection.
pub fn sync_keybinding_warning_notification(
    store: &crate::state::store::AppStore,
    warnings: &[crate::keybindings::validate::KeybindingWarning],
) {
    use crate::context::notifications::{
        Notification, NotificationColor, NotificationPriority, NotificationsWriter,
    };
    use crate::keybindings::types::KeybindingWarningSeverity;

    let mut writer = NotificationsWriter::new(store.clone());
    if warnings.is_empty() {
        writer.remove_notification(WARNING_NOTIFICATION_KEY);
        return;
    }
    let errors = warnings
        .iter()
        .filter(|warning| warning.severity == KeybindingWarningSeverity::Error)
        .count();
    let warning_count = warnings.len() - errors;
    let plural = |count: usize, singular: &str| {
        if count == 1 {
            singular.to_string()
        } else {
            format!("{singular}s")
        }
    };
    let message = if errors > 0 && warning_count > 0 {
        format!(
            "Found {errors} keybinding {} and {warning_count} {}",
            plural(errors, "error"),
            plural(warning_count, "warning")
        )
    } else if errors > 0 {
        format!("Found {errors} keybinding {}", plural(errors, "error"))
    } else {
        format!(
            "Found {warning_count} keybinding {}",
            plural(warning_count, "warning")
        )
    } + " · /doctor for details";
    writer.add_notification(
        Notification::text(
            WARNING_NOTIFICATION_KEY,
            message,
            if errors > 0 {
                NotificationPriority::Immediate
            } else {
                NotificationPriority::High
            },
        )
        .with_color(if errors > 0 {
            NotificationColor::Error
        } else {
            NotificationColor::Warning
        })
        .with_timeout_ms(60_000),
    );
}

#[derive(Default, Props)]
pub struct KeybindingSetupProps<'a> {
    /// CC `Props.children` (`KeybindingProviderSetup.tsx:40-42`).
    pub children: Vec<AnyElement<'a>>,
}

/// Maps to: CC `KeybindingProviderSetup.tsx:106-238` `KeybindingSetup`.
///
/// Each mount owns its own runtime — bindings, handler registry, active
/// contexts and pending chord — as each CC mount owns its state and refs:
/// REPL, every setup dialog and the resume chooser mount their own. The
/// bindings come from `loadKeybindingsSyncWithWarnings()` (:108-115), served
/// from the loader's cache (filled at startup), so a mount reads no file
/// inside a frame. A reload reaches every mount through
/// `subscribeToKeybindingChanges` (:198-218).
///
/// A raw `Component`, not a `#[component]` fn, for the pass-through reason
/// `McpConnectionManager` documents: children go to `update_children` by
/// `iter_mut()` on every update.
#[derive(Default)]
pub struct KeybindingSetup;

impl Component for KeybindingSetup {
    type Props<'a> = KeybindingSetupProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        let mut hooks = hooks.with_context_stack(updater.component_context_stack());
        let runtime = use_keybinding_setup_instance(&mut hooks);
        updater.set_transparent_layout(true);
        updater.update_children(
            props.children.iter_mut(),
            Some(Context::owned(runtime).borrow()),
        );
    }
}

/// The state and effects of one `KeybindingSetup` mount.
fn use_keybinding_setup_instance(hooks: &mut Hooks) -> KeybindingRuntime {
    use futures::StreamExt as _;
    use std::sync::{Arc, Mutex};

    // CC :108-115 `useState(() => loadKeybindingsSyncWithWarnings())`.
    let runtime = hooks.use_const(|| {
        KeybindingRuntime::new(
            super::load_user_bindings::load_keybindings_sync_with_warnings().bindings,
        )
    });
    // CC `useKeybindingWarnings` writes through `useNotifications`, i.e. the
    // enclosing AppState; a mount outside a provider has nowhere to show it.
    let store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    // CC :121 `useKeybindingWarnings(warnings, isReload)` for the mount-time
    // load result.
    hooks.use_effect(
        {
            let store = store.clone();
            move || {
                if let Some(store) = store.as_ref() {
                    sync_keybinding_warning_notification(
                        store,
                        &super::load_user_bindings::get_cached_keybinding_warnings(),
                    );
                }
            }
        },
        (),
    );
    // CC :198-218 — subscribe on mount, unsubscribe on unmount (dropping the
    // subscription). A reload replaces this mount's bindings and re-renders
    // the subtree, as CC's `setLoadResult` does.
    let changes = hooks.use_const(|| {
        let (subscription, receiver) =
            super::load_user_bindings::subscribe_to_keybinding_changes();
        Arc::new((subscription, Mutex::new(Some(receiver))))
    });
    let mut reloads = hooks.use_state(|| 0u64);
    hooks.use_future({
        let receiver = changes.1.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        let runtime = runtime.clone();
        let store = store.clone();
        async move {
            let Some(mut receiver) = receiver else {
                return;
            };
            while let Some(result) = receiver.next().await {
                runtime.replace_bindings(result.bindings);
                if let Some(store) = store.as_ref() {
                    sync_keybinding_warning_notification(store, &result.warnings);
                }
                reloads.set(reloads.get().wrapping_add(1));
            }
        }
    });
    // CC :231-237 `<ChordInterceptor>`. This component's own hooks are
    // polled after its children, so the interceptor sees each key after
    // every consumer below — the order the bypass listener relies on.
    use_chord_interceptor(hooks, &runtime);
    runtime
}

/// Maps to: CC `ChordInterceptor` (`KeybindingProviderSetup.tsx:259-381`) as a
/// bypass listener: it observes every key regardless of downstream
/// consumption; consumers check `chord_pending()` themselves.
fn use_chord_interceptor(hooks: &mut Hooks, runtime: &KeybindingRuntime) {
    hooks.use_terminal_events({
        let runtime = runtime.clone();
        move |event| {
            if let TerminalEvent::Key(key_event) = &event {
                if let Some(keystroke) = key_event_to_keystroke(key_event) {
                    runtime.observe_keystroke(&keystroke);
                }
            }
        }
    });
}

/// A runtime handed in by the caller plus the ChordInterceptor, for test
/// harnesses that build their own runtime. Production mounts `KeybindingSetup`.
pub fn use_keybinding_setup(
    hooks: &mut Hooks,
    startup_runtime: KeybindingRuntime,
) -> KeybindingRuntime {
    let runtime_state = hooks.use_state(move || startup_runtime);
    let runtime = runtime_state.read().clone();
    use_chord_interceptor(hooks, &runtime);
    runtime
}

/// Test harness provider for component-level single-key action tests. Chord
/// interceptor tests mount `use_keybinding_setup` explicitly.
#[cfg(test)]
pub fn test_keybinding_root(child: AnyElement<'static>) -> AnyElement<'static> {
    element! {
        ContextProvider(value: Context::owned(KeybindingRuntime::with_default_bindings())) {
            #(vec![child])
        }
    }
    .into_any()
}

#[cfg(test)]
mod setup_mount_tests {
    use super::*;
    use crate::keybindings::load_user_bindings::{
        KeybindingsLoadResult, emit_keybinding_change_for_testing,
        keybinding_change_subscriber_count_for_testing, set_cached_keybindings_for_testing,
    };
    use crate::keybindings::types::{ContextName, ParsedBinding};
    use futures::StreamExt as _;

    /// Shows the transcript-toggle shortcut from the nearest runtime, as
    /// CC's `useShortcutDisplay` does.
    #[component]
    fn ShortcutProbe(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let shown = hooks
            .try_use_context::<KeybindingRuntime>()
            .map(|runtime| {
                crate::keybindings::shortcut_format::get_shortcut_display_from_bindings(
                    "app:toggleTranscript",
                    &ContextName::Global,
                    "none",
                    runtime.bindings().as_slice(),
                )
            })
            .unwrap_or_else(|| "no runtime".to_string());
        element! { Text(content: format!("toggle={shown}")) }
    }

    #[test]
    fn keybinding_setup_rerenders_its_subtree_with_reloaded_bindings() {
        // CC KeybindingProviderSetup.tsx:198-218: a reload reaches every
        // mount's subscription and `setLoadResult` re-renders its consumers.
        // Frame-driven: take the mount frame, emit, take the next frame.
        set_cached_keybindings_for_testing(crate::keybindings::default_bindings::default_bindings());
        let before = keybinding_change_subscriber_count_for_testing();
        let (mounted, subscribed, reloaded) = futures::executor::block_on(async {
            let mut app = element! {
                KeybindingSetup {
                    ShortcutProbe
                }
            };
            let frames = app.mock_terminal_render_loop(MockTerminalConfig::default().with_size(40, 3));
            futures::pin_mut!(frames);
            let mounted = frames.next().await.expect("mount frame").to_string();
            let subscribed = keybinding_change_subscriber_count_for_testing();
            let mut bindings = crate::keybindings::default_bindings::default_bindings();
            bindings.push(ParsedBinding {
                chord: crate::keybindings::parser::parse_chord("ctrl+t"),
                action: Some("app:toggleTranscript".to_string()),
                context: ContextName::Global,
            });
            emit_keybinding_change_for_testing(KeybindingsLoadResult {
                bindings,
                warnings: Vec::new(),
            });
            let reloaded = frames.next().await.expect("frame after the reload").to_string();
            (mounted, subscribed, reloaded)
        });
        assert!(mounted.contains("toggle=ctrl+o"), "mount frame:\n{mounted}");
        assert_eq!(subscribed, before + 1, "the mount subscribes to reloads");
        assert!(reloaded.contains("toggle=ctrl+t"), "frame after the reload:\n{reloaded}");
    }

    #[test]
    fn keybinding_setup_unsubscribes_when_it_unmounts() {
        // CC :213-216: the effect's cleanup calls `unsubscribe()`.
        let before = keybinding_change_subscriber_count_for_testing();
        let text = element! {
            KeybindingSetup {
                ShortcutProbe
            }
        }
        .render(Some(40))
        .to_string();
        assert!(text.contains("toggle="), "canvas=\n{text}");
        assert!(!text.contains("no runtime"), "the mount provides a runtime; canvas=\n{text}");
        assert_eq!(keybinding_change_subscriber_count_for_testing(), before);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::notifications::{NotificationColor, NotificationPriority};
    use crate::keybindings::validate::KeybindingWarning;
    use crate::state::app_state_store::AppState;
    use crate::state::store::AppStore;

    #[test]
    fn warning_projection_matches_official_notification_copy_and_priority() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let store = AppStore::new(AppState::default(), None);
        sync_keybinding_warning_notification(
            &store,
            &[
                KeybindingWarning::error("bad binding", None::<String>),
                KeybindingWarning::warning("risky binding", None::<String>),
            ],
        );
        let current = store
            .get()
            .notifications
            .current
            .clone()
            .expect("immediate keybinding warning");
        assert_eq!(
            current.text,
            "Found 1 keybinding error and 1 warning · /doctor for details"
        );
        assert_eq!(current.priority, NotificationPriority::Immediate);
        assert_eq!(current.color, Some(NotificationColor::Error));
        assert_eq!(current.timeout_ms, Some(60_000));

        sync_keybinding_warning_notification(&store, &[]);
        assert!(store.get().notifications.current.is_none());
    }
}
