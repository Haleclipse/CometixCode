//! Maps to: CC `components/PromptInput/SandboxPromptFooterHint.tsx`.
//!
//! Owns the violation-store subscription seam + 5s decay (CC local state),
//! reads sandbox-enabled + shortcut live, and bumps the PromptInput-scoped
//! [`FooterLayoutWake`] when the recent count changes (L1 `PromptInput-scoped
//! footer layout wake`, PORTING.md) — AppStore is not involved in layout
//! wakes.
//!
//! S9-1 ruling (review blocker, resolved 2026-08-02): the recent count's
//! single source is the module-local `SANDBOX_HINT_RECENT` atomic — both the
//! height helpers AND the render text derive from it. The previous
//! component-local `State<u32>` payload was `.set()` from the violation
//! recorder's thread and the tokio decay task — the same cross-thread
//! `try_write` silent-drop class the wake carrier's CAUTION ruling
//! eliminated; a dropped decay `set(0)` left stale hint text with a zero
//! height budget, unbounded until the next violation. With the atomic as the
//! only payload, no `State` handle is reachable from the listener/decay
//! closures (see `schedule_sandbox_hint_decay`'s signature), so the loss
//! class is impossible by construction. Because the render text now comes
//! from the atomic, [`set_sandbox_hint_recent`] bumps the wake on every
//! count CHANGE (CC's per-violation `setRecentViolationCount` re-render
//! analog) — still event-driven, never periodic; visibility flips are a
//! subset.

use super::footer_layout_wake::FooterLayoutWake;
use super::prompt_input_footer::PromptFooterIndicator;
use crate::keybindings::shortcut_format::get_shortcut_display_for_context_name;
use crate::state::store::AppStore;
use crate::utils::sandbox::sandbox_adapter::{
    SandboxViolationListenerId, get_sandbox_enabled_setting, get_sandbox_violation_store,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Live recent-violation count for PromptInput height (D9).
static SANDBOX_HINT_RECENT: AtomicU32 = AtomicU32::new(0);

pub fn sandbox_hint_recent_count() -> u32 {
    SANDBOX_HINT_RECENT.load(Ordering::Relaxed)
}

fn set_sandbox_hint_recent(count: u32, wake: Option<&FooterLayoutWake>) {
    let prev = SANDBOX_HINT_RECENT.swap(count, Ordering::Relaxed);
    // S9-1: the atomic is the single source for height AND render text, so
    // any count change must re-render (not just visibility flips) — mirrors
    // CC's per-violation setState re-render. Event-driven only; a repeated
    // same-count write (e.g. decay after already-hidden) stays silent.
    if prev != count
        && let Some(wake) = wake
    {
        wake.bump();
    }
}

/// Test-only: set recent count without an AppStore bump.
#[cfg(test)]
pub fn set_sandbox_hint_recent_for_test(count: u32) {
    SANDBOX_HINT_RECENT.store(count, Ordering::Relaxed);
}

/// Shared row-text logic for both the component and tests.
pub(crate) fn sandbox_hint_text(
    sandboxing_enabled: bool,
    recent_violation_count: u32,
    details_shortcut: Option<&str>,
) -> Option<String> {
    if !sandboxing_enabled || recent_violation_count == 0 {
        return None;
    }

    let operation = if recent_violation_count == 1 {
        "operation"
    } else {
        "operations"
    };
    let details_shortcut = details_shortcut
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ctrl+o");

    Some(format!(
        "⧈ Sandbox blocked {recent_violation_count} {operation} · {details_shortcut} for details · /sandbox to disable"
    ))
}

/// Payload helper retained for tests / transitional callers.
pub fn sandbox_violation_indicator(
    sandboxing_enabled: bool,
    recent_violation_count: u32,
    details_shortcut: Option<&str>,
) -> Option<PromptFooterIndicator> {
    sandbox_hint_text(sandboxing_enabled, recent_violation_count, details_shortcut)
        .map(PromptFooterIndicator::inactive)
}

/// Maps to: CC footer height contribution of `<SandboxPromptFooterHint />`.
pub fn sandbox_hint_row_count(sandboxing_enabled: bool, recent_violation_count: u32) -> usize {
    usize::from(sandbox_hint_text(sandboxing_enabled, recent_violation_count, None).is_some())
}

fn details_shortcut_display() -> String {
    get_shortcut_display_for_context_name("app:toggleTranscript", "Global", "ctrl+o")
}

struct SandboxListenerGuard(Option<SandboxViolationListenerId>);

impl Drop for SandboxListenerGuard {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            get_sandbox_violation_store().unsubscribe(id);
        }
    }
}

static SANDBOX_HINT_DECAY: OnceLock<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> =
    OnceLock::new();

fn sandbox_decay_slot() -> &'static Mutex<Option<tokio::sync::oneshot::Sender<()>>> {
    SANDBOX_HINT_DECAY.get_or_init(|| Mutex::new(None))
}

/// S9-1 type surface: no iocraft `State` parameter — the decay task writes
/// only the atomic (via [`set_sandbox_hint_recent`]) and bumps the lossless
/// wake, so no cross-thread `State::set` exists on this path.
fn schedule_sandbox_hint_decay(wake: Option<FooterLayoutWake>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Ok(mut guard) = sandbox_decay_slot().lock() {
        if let Some(prev) = guard.take() {
            let _ = prev.send(());
        }
        *guard = Some(tx);
    }
    tokio::spawn(async move {
        tokio::select! {
            _ = futures_timer::Delay::new(Duration::from_secs(5)) => {
                set_sandbox_hint_recent(0, wake.as_ref());
            }
            _ = rx => {}
        }
        if let Ok(mut guard) = sandbox_decay_slot().lock() {
            *guard = None;
        }
    });
}

#[derive(Default, Props)]
pub struct SandboxPromptFooterHintProps {
    /// Test-only override. Production leaves this `None` and self-reads.
    pub sandboxing_enabled: Option<bool>,
    /// Test-only override for the recent count. Production leaves this `None`.
    pub recent_violation_count: Option<u32>,
    /// Test-only override for the details shortcut.
    pub details_shortcut: Option<String>,
}

/// Maps to: CC `SandboxPromptFooterHint` — self-subscribing footer hint.
#[component]
pub fn SandboxPromptFooterHint(
    props: &SandboxPromptFooterHintProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let store = hooks.try_use_context::<AppStore>().map(|s| s.clone());
    // PromptInput-scoped layout wake (absent in isolated test mounts).
    let footer_layout_wake = hooks
        .try_use_context::<FooterLayoutWake>()
        .map(|wake| wake.clone());

    let live_enabled = store
        .as_ref()
        .map(|store| get_sandbox_enabled_setting(&store.get().settings))
        .unwrap_or(false);
    let sandboxing_enabled = props.sandboxing_enabled.unwrap_or(live_enabled);

    // Production: subscribe once. Tests that pass an explicit count skip the
    // store subscription so unit canvases stay deterministic. The AppStore is
    // used for the sandbox-enabled settings read only; the payload is the
    // module atomic and layout/text wakes go through the scoped
    // FooterLayoutWake (S9-1: the listener closure holds no iocraft `State`
    // handle, so no cross-thread `State::set` can occur).
    let _listener = hooks.use_state(|| {
        if props.recent_violation_count.is_some() {
            return SandboxListenerGuard(None);
        }
        let Some(store) = store.clone() else {
            return SandboxListenerGuard(None);
        };
        let store_for_listener = store.clone();
        let wake_for_listener = footer_layout_wake.clone();
        let id = get_sandbox_violation_store().subscribe(move |_total, delta| {
            if delta == 0 {
                return;
            }
            let enabled = get_sandbox_enabled_setting(&store_for_listener.get().settings);
            if !enabled {
                return;
            }
            // Atomic recent + count-change wake; the wake re-renders the
            // PromptInput subtree, which re-reads the atomic below.
            set_sandbox_hint_recent(delta, wake_for_listener.as_ref());
            schedule_sandbox_hint_decay(wake_for_listener.clone());
        });
        SandboxListenerGuard(Some(id))
    });

    // S9-1: render text derives from the same atomic the height helpers read
    // — single source, so height budget and rendered text cannot diverge.
    let recent_violation_count = props
        .recent_violation_count
        .unwrap_or_else(sandbox_hint_recent_count);
    let details_shortcut = props
        .details_shortcut
        .clone()
        .unwrap_or_else(details_shortcut_display);

    match sandbox_hint_text(
        sandboxing_enabled,
        recent_violation_count,
        Some(details_shortcut.as_str()),
    ) {
        Some(text) => element! {
            View(flex_direction: FlexDirection::Row, height: 1u32, overflow: Overflow::Hidden) {
                Text(content: text, color: theme.inactive, wrap: TextWrap::Truncate)
            }
        }
        .into_any(),
        None => element! { View(width: 0u32, height: 0u32) }.into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    /// Serializes tests that manipulate the module-global
    /// `SANDBOX_HINT_RECENT` atomic so parallel scheduling cannot interleave
    /// their flip sequences.
    static SANDBOX_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn sandbox_violation_indicator_matches_official_copy_and_gates() {
        assert!(sandbox_violation_indicator(false, 1, Some("ctrl+o")).is_none());
        assert!(sandbox_violation_indicator(true, 0, Some("ctrl+o")).is_none());

        let singular = sandbox_violation_indicator(true, 1, Some("ctrl+shift+o")).unwrap();
        assert_eq!(
            singular.text,
            "⧈ Sandbox blocked 1 operation · ctrl+shift+o for details · /sandbox to disable"
        );
        assert!(singular.segments[0].color.is_none());
        assert!(!singular.segments[0].dim);

        let plural = sandbox_violation_indicator(true, 3, None).unwrap();
        assert_eq!(
            plural.text,
            "⧈ Sandbox blocked 3 operations · ctrl+o for details · /sandbox to disable"
        );
    }

    #[test]
    fn sandbox_component_renders_row_and_hides_when_gated() {
        let visible = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SandboxPromptFooterHint(
                    sandboxing_enabled: Some(true),
                    recent_violation_count: Some(2u32),
                    details_shortcut: Some("ctrl+o".to_string()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(
            visible.contains(
                "⧈ Sandbox blocked 2 operations · ctrl+o for details · /sandbox to disable"
            ),
            "canvas=\n{visible}"
        );

        let hidden = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SandboxPromptFooterHint(
                    sandboxing_enabled: Some(false),
                    recent_violation_count: Some(2u32),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(hidden.trim().is_empty(), "canvas=\n{hidden}");
    }

    #[test]
    fn sandbox_hint_row_count_matches_visibility_gate() {
        assert_eq!(sandbox_hint_row_count(false, 2), 0);
        assert_eq!(sandbox_hint_row_count(true, 0), 0);
        assert_eq!(sandbox_hint_row_count(true, 2), 1);
    }

    #[test]
    fn sandbox_atomic_feeds_direct_footer_height() {
        use crate::state::app_state_store::AppState;
        use crate::state::store::AppStore;
        use crate::utils::settings::SettingsJson;

        let _guard = SANDBOX_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let row_count_for = |store: &AppStore| {
            crate::components::prompt_input::notifications::direct_footer_row_count_from_app(
                &store.get(),
                false,
                false,
                &crate::hooks::use_credential_status::CredentialStatus::Ready,
                // No messages in this fixture; zero usage matches an empty
                // conversation (CC derives tokenUsage from messages).
                0,
                // No auto-updater result / install in flight in this fixture
                // (owner-threaded values, CC Notifications.tsx:53-55 props).
                None,
                false,
                // No REPL ide selection in this fixture (owner-threaded value,
                // CC Notifications.tsx:60,75 props).
                None,
            )
        };

        set_sandbox_hint_recent_for_test(2);
        let store = AppStore::new(AppState::default(), None);
        let mut settings = SettingsJson::default();
        settings.sandbox = Some(serde_json::json!({ "enabled": true }));
        store.replace_with(|s| s.settings = std::sync::Arc::new(settings));
        assert_eq!(sandbox_hint_recent_count(), 2);
        // 0→1 visibility flip changes the height budget…
        assert_eq!(row_count_for(&store), 1);
        assert_eq!(
            crate::components::prompt_input::prompt_input_footer::bridge_status_indicator_count_from_app(
                &store.get(),
                // Even with both live gates passing, the pill needs a
                // repl_bridge_enabled producer (absent in production).
                true,
                || true,
            ),
            0
        );
        // …and the 1→0 flip drops it back.
        set_sandbox_hint_recent_for_test(0);
        assert_eq!(row_count_for(&store), 0);
    }

    /// Pins the slice-9 performance fix: sandbox count changes bump the
    /// PromptInput-scoped wake only — AppStore `on_change` and listeners are
    /// NOT invoked by layout/text wakes (previously every flip woke the whole
    /// tree through the version counter). Post-S9-1, visible→visible count
    /// changes also bump (the render text derives from the atomic), while
    /// same-count writes stay silent — still event-driven, never periodic.
    #[test]
    fn sandbox_count_changes_bump_scoped_wake_without_touching_app_store() {
        use crate::state::app_state_store::AppState;
        use crate::state::store::AppStore;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let _guard = SANDBOX_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let on_change_calls = Arc::new(AtomicUsize::new(0));
        let on_change_counter = Arc::clone(&on_change_calls);
        let store = AppStore::new(
            AppState::default(),
            Some(Arc::new(move |_new: &AppState, _old: &AppState| {
                on_change_counter.fetch_add(1, Ordering::SeqCst);
            })),
        );
        let listener_calls = Arc::new(AtomicUsize::new(0));
        let listener_counter = Arc::clone(&listener_calls);
        let _listener = store.subscribe(Arc::new(move || {
            listener_counter.fetch_add(1, Ordering::SeqCst);
        }));

        let wake = FooterLayoutWake::detached();
        set_sandbox_hint_recent_for_test(0);
        set_sandbox_hint_recent(2, Some(&wake));
        assert_eq!(wake.epoch(), 1, "0→N flip must bump the scoped wake");
        set_sandbox_hint_recent(3, Some(&wake));
        assert_eq!(
            wake.epoch(),
            2,
            "count change while visible must bump (render text reads the atomic)"
        );
        set_sandbox_hint_recent(3, Some(&wake));
        assert_eq!(wake.epoch(), 2, "same-count write must stay silent");
        set_sandbox_hint_recent(0, Some(&wake));
        assert_eq!(wake.epoch(), 3, "N→0 flip must bump the scoped wake");
        set_sandbox_hint_recent(0, Some(&wake));
        assert_eq!(
            wake.epoch(),
            3,
            "decay after already-hidden must stay silent"
        );

        assert_eq!(
            on_change_calls.load(Ordering::SeqCst),
            0,
            "layout/text wakes must not invoke AppStore on_change"
        );
        assert_eq!(
            listener_calls.load(Ordering::SeqCst),
            0,
            "layout/text wakes must not notify AppStore listeners"
        );
    }

    /// S9-1 pin: the dropped-State-write divergence is impossible by
    /// construction — height budget and render text derive from the same
    /// `SANDBOX_HINT_RECENT` atomic (no iocraft `State` payload exists; the
    /// listener/decay closures capture only the wake handle, see
    /// `schedule_sandbox_hint_decay`'s State-free signature). Exercises the
    /// exact review scenario: decay-to-0 must hide the text in the same step
    /// that zeroes the height budget.
    #[test]
    fn sandbox_height_and_render_text_share_single_atomic_source() {
        let _guard = SANDBOX_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wake = FooterLayoutWake::detached();

        set_sandbox_hint_recent_for_test(0);
        set_sandbox_hint_recent(2, Some(&wake));
        let live = sandbox_hint_recent_count();
        assert_eq!(sandbox_hint_row_count(true, live), 1);
        assert!(
            sandbox_hint_text(true, live, None).is_some(),
            "visible: text and height must agree from the shared atomic"
        );

        // Review S9-1 scenario: the 5s decay writes 0. Previously the State
        // text source could silently miss this write while the atomic height
        // source took it (stale text over a zero-height row). Single source
        // now: one write updates both.
        set_sandbox_hint_recent(0, Some(&wake));
        let live = sandbox_hint_recent_count();
        assert_eq!(sandbox_hint_row_count(true, live), 0);
        assert!(
            sandbox_hint_text(true, live, None).is_none(),
            "hidden: text and height must agree from the shared atomic"
        );
    }
}
