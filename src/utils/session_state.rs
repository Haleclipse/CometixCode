//! Maps to: CC `utils/sessionState.ts`.
//!
//! A process-wide registry of three nullable listener slots plus the session's
//! current state. CC keeps these as module-level `let` bindings; the Rust
//! equivalent is a `OnceLock<Mutex<Option<_>>>` per slot, matching the shape
//! `utils/sandbox/sandbox_adapter.rs`'s `SANDBOX_ASK_CALLBACK` already uses.
//!
//! Lock discipline: a listener is cloned out and the lock released BEFORE the
//! callback runs. Same rule Contract A clause 2 puts on the AppStore listener
//! registry ("listener registry lock 不跨任何 callback") — a callback that
//! re-entered a notify would otherwise deadlock on a non-reentrant `Mutex`.

use crate::types::permissions::PermissionMode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Maps to: CC `:1` `SessionState = 'idle' | 'running' | 'requires_action'`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionState {
    #[default]
    Idle,
    Running,
    RequiresAction,
}

impl SessionState {
    /// The wire discriminants CC's union is made of.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::RequiresAction => "requires_action",
        }
    }
}

/// Maps to: CC `:15-24 RequiresActionDetails`.
///
/// Context carried with `requires_action` transitions so downstream surfaces
/// (CCR sidebar, push notifications) can show what the session is blocked on,
/// not just that it is blocked. CC documents two delivery paths for it:
/// `tool_name` + `action_description` become the typed proto payload, and the
/// whole object goes into `external_metadata.pending_action` as queryable JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct RequiresActionDetails {
    pub tool_name: String,
    /// Human-readable summary, e.g. "Editing src/foo.ts", "Running npm test".
    pub action_description: String,
    pub tool_use_id: String,
    pub request_id: String,
    /// Raw tool input — the frontend reads
    /// `external_metadata.pending_action.input` to parse question options /
    /// plan content without scanning the event stream.
    pub input: Option<serde_json::Value>,
}

/// Maps to: CC `:32-46 SessionExternalMetadata` — the CCR `external_metadata`
/// keys, pushed in `onChangeAppState` and restored by
/// `externalMetadataToAppState`.
///
/// Every field is RFC 7396 three-state, which `Option<Option<T>>` is here for:
/// - `None` — key absent, leave the stored value alone
/// - `Some(None)` — explicit JSON `null`, clear the stored value
/// - `Some(Some(v))` — set
///
/// The distinction is load-bearing, not decorative: `notify_session_state_changed`
/// clears `pending_action` by sending an explicit null (CC `:107`) while leaving
/// every other key absent.
///
/// `post_turn_summary` is the one field CC types without `| null`
/// (`post_turn_summary?: unknown`), so it is a plain `Option` — absent or set,
/// never explicitly nulled. CC keeps it opaque on purpose: typing it would leak
/// the import path into `sdk.d.ts` through agentSdkBridge's re-export.
///
/// No serde derive yet, deliberately: CC's `sessionState.ts` declares this type
/// and nothing more — the JSON round-trip lives at the consumers
/// (`cli/transports/ccrClient.ts`), which are not ported. Adding a serialization
/// policy here would be inventing one ahead of its call site, and the
/// `Option<Option<T>>` null-vs-absent mapping is exactly the kind of decision
/// that must be made against a real consumer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionExternalMetadata {
    pub permission_mode: Option<Option<String>>,
    pub is_ultraplan_mode: Option<Option<bool>>,
    pub model: Option<Option<String>>,
    pub pending_action: Option<Option<RequiresActionDetails>>,
    /// Opaque — typed at the emit site (CC `:38-41`).
    pub post_turn_summary: Option<serde_json::Value>,
    /// Mid-turn progress line from the forked-agent summarizer.
    pub task_summary: Option<Option<String>>,
}

/// Maps to: CC `:48-51`.
pub type SessionStateChangedListener =
    Arc<dyn Fn(SessionState, Option<&RequiresActionDetails>) + Send + Sync>;
/// Maps to: CC `:52-54`.
pub type SessionMetadataChangedListener = Arc<dyn Fn(&SessionExternalMetadata) + Send + Sync>;
/// Maps to: CC `:55`.
pub type PermissionModeChangedListener = Arc<dyn Fn(PermissionMode) + Send + Sync>;

fn state_listener_slot() -> &'static Mutex<Option<SessionStateChangedListener>> {
    static SLOT: OnceLock<Mutex<Option<SessionStateChangedListener>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn metadata_listener_slot() -> &'static Mutex<Option<SessionMetadataChangedListener>> {
    static SLOT: OnceLock<Mutex<Option<SessionMetadataChangedListener>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn permission_mode_listener_slot() -> &'static Mutex<Option<PermissionModeChangedListener>> {
    static SLOT: OnceLock<Mutex<Option<PermissionModeChangedListener>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Maps to: CC `:57` `let hasPendingAction = false`.
static HAS_PENDING_ACTION: AtomicBool = AtomicBool::new(false);

fn current_state_slot() -> &'static Mutex<SessionState> {
    static SLOT: OnceLock<Mutex<SessionState>> = OnceLock::new();
    // Maps to: CC `:58` `let currentState: SessionState = 'idle'`.
    SLOT.get_or_init(|| Mutex::new(SessionState::Idle))
}

/// Take a clone of the listener and drop the lock, so the callback never runs
/// under it.
fn take_metadata_listener() -> Option<SessionMetadataChangedListener> {
    metadata_listener_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Maps to: CC `:60-64 setSessionStateChangedListener`.
pub fn set_session_state_changed_listener(cb: Option<SessionStateChangedListener>) {
    *state_listener_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = cb;
}

/// Maps to: CC `:66-70 setSessionMetadataChangedListener`.
pub fn set_session_metadata_changed_listener(cb: Option<SessionMetadataChangedListener>) {
    *metadata_listener_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = cb;
}

/// Maps to: CC `:79-83 setPermissionModeChangedListener`.
///
/// Wired by `cli/print.ts` to emit an SDK `system:status` message so CCR/IDE
/// clients see mode transitions in real time — regardless of which code path
/// mutated `toolPermissionContext.mode` (Shift+Tab, ExitPlanMode dialog, slash
/// command, bridge `set_permission_mode`, …).
pub fn set_permission_mode_changed_listener(cb: Option<PermissionModeChangedListener>) {
    *permission_mode_listener_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = cb;
}

/// Maps to: CC `:88-90 getSessionState`.
pub fn get_session_state() -> SessionState {
    *current_state_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Maps to: CC `:92-134 notifySessionStateChanged`.
pub fn notify_session_state_changed(state: SessionState, details: Option<&RequiresActionDetails>) {
    // CC `:97`.
    *current_state_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = state;

    // CC `:98` — `stateListener?.(state, details)`.
    let state_listener = state_listener_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(listener) = state_listener {
        listener(state, details);
    }

    // CC `:103-112` — mirror the details into external_metadata, and clear them
    // with an RFC 7396 null on the next non-blocked transition.
    match (state, details) {
        (SessionState::RequiresAction, Some(details)) => {
            HAS_PENDING_ACTION.store(true, Ordering::SeqCst);
            if let Some(listener) = take_metadata_listener() {
                listener(&SessionExternalMetadata {
                    pending_action: Some(Some(details.clone())),
                    ..SessionExternalMetadata::default()
                });
            }
        }
        _ => {
            // CC `:109` — `else if (hasPendingAction)`.
            if HAS_PENDING_ACTION.swap(false, Ordering::SeqCst) {
                if let Some(listener) = take_metadata_listener() {
                    listener(&SessionExternalMetadata {
                        pending_action: Some(None),
                        ..SessionExternalMetadata::default()
                    });
                }
            }
        }
    }

    // CC `:116-118` — task_summary is written mid-turn by the forked
    // summarizer; clear it at idle so the next turn does not briefly show the
    // previous turn's progress.
    if state == SessionState::Idle {
        if let Some(listener) = take_metadata_listener() {
            listener(&SessionExternalMetadata {
                task_summary: Some(None),
                ..SessionExternalMetadata::default()
            });
        }
    }

    // SEAM: CC `:127-133` mirrors the transition onto the SDK event stream
    // behind `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS`, via
    // `enqueueSdkEvent({type: 'system', subtype: 'session_state_changed', state})`.
    // `utils/sdkEventQueue.ts` has no Rust counterpart yet, so the gate is read
    // but the emit cannot be made. The env check is kept so the seam is visible
    // at the exact branch CC emits from rather than only in this comment.
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS")
            .as_deref(),
    ) {
        tracing::debug!(
            state = state.as_str(),
            "CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS is set but utils/sdkEventQueue.ts is unported"
        );
    }
}

/// Maps to: CC `:136-140 notifySessionMetadataChanged`.
pub fn notify_session_metadata_changed(metadata: &SessionExternalMetadata) {
    if let Some(listener) = take_metadata_listener() {
        listener(metadata);
    }
}

/// Maps to: CC `:148-150 notifyPermissionModeChanged`.
///
/// Fired by `onChangeAppState` when `toolPermissionContext.mode` changes.
/// Downstream listeners (CCR external_metadata PUT, SDK status stream) are both
/// wired through this single choke point so no mode-mutation path can silently
/// bypass them.
pub fn notify_permission_mode_changed(mode: PermissionMode) {
    let listener = permission_mode_listener_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(listener) = listener {
        listener(mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// The registry is process-wide, so these tests must not interleave.
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn reset() {
        set_session_state_changed_listener(None);
        set_session_metadata_changed_listener(None);
        set_permission_mode_changed_listener(None);
        HAS_PENDING_ACTION.store(false, Ordering::SeqCst);
        *current_state_slot()
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = SessionState::Idle;
    }

    fn recording_metadata_listener() -> (
        SessionMetadataChangedListener,
        Arc<StdMutex<Vec<SessionExternalMetadata>>>,
    ) {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink = seen.clone();
        let listener: SessionMetadataChangedListener = Arc::new(move |metadata| {
            sink.lock().unwrap().push(metadata.clone());
        });
        (listener, seen)
    }

    fn details() -> RequiresActionDetails {
        RequiresActionDetails {
            tool_name: "Edit".to_string(),
            action_description: "Editing src/foo.rs".to_string(),
            tool_use_id: "toolu_1".to_string(),
            request_id: "req_1".to_string(),
            input: None,
        }
    }

    /// Maps to: CC `:97` + `:88-90` — the notify writes the state the getter
    /// reads.
    #[test]
    fn notify_updates_the_state_the_getter_returns() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset();

        notify_session_state_changed(SessionState::Running, None);

        assert_eq!(get_session_state(), SessionState::Running);
    }

    /// Maps to: CC `:103-108` — a `requires_action` transition carrying details
    /// mirrors them into `pending_action`.
    #[test]
    fn requires_action_mirrors_details_into_pending_action() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset();
        let (listener, seen) = recording_metadata_listener();
        set_session_metadata_changed_listener(Some(listener));

        notify_session_state_changed(SessionState::RequiresAction, Some(&details()));

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].pending_action, Some(Some(details())));
        reset();
    }

    /// Maps to: CC `:109-112` — the NEXT non-blocked transition clears it with
    /// an explicit RFC 7396 null, which is `Some(None)` and NOT `None`.
    #[test]
    fn leaving_requires_action_clears_pending_action_with_an_explicit_null() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset();
        let (listener, seen) = recording_metadata_listener();
        set_session_metadata_changed_listener(Some(listener));

        notify_session_state_changed(SessionState::RequiresAction, Some(&details()));
        notify_session_state_changed(SessionState::Running, None);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[1].pending_action,
            Some(None),
            "explicit null clears; absent would leave the stored value alone"
        );
        drop(seen);
        reset();
    }

    /// Maps to: CC `:109` — the clear only fires when something was pending,
    /// so a plain running→running transition emits nothing.
    #[test]
    fn transitions_without_a_pending_action_emit_no_metadata() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset();
        let (listener, seen) = recording_metadata_listener();
        set_session_metadata_changed_listener(Some(listener));

        notify_session_state_changed(SessionState::Running, None);

        assert!(seen.lock().unwrap().is_empty());
        reset();
    }

    /// Maps to: CC `:116-118` — idle additionally clears `task_summary`.
    #[test]
    fn idle_clears_task_summary() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset();
        let (listener, seen) = recording_metadata_listener();
        set_session_metadata_changed_listener(Some(listener));

        notify_session_state_changed(SessionState::Idle, None);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].task_summary, Some(None));
        drop(seen);
        reset();
    }

    /// Maps to: CC `:148-150` — the permission-mode choke point.
    #[test]
    fn permission_mode_listener_receives_the_mode() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink = seen.clone();
        set_permission_mode_changed_listener(Some(Arc::new(move |mode| {
            sink.lock().unwrap().push(mode);
        })));

        notify_permission_mode_changed(PermissionMode::AcceptEdits);

        assert_eq!(*seen.lock().unwrap(), vec![PermissionMode::AcceptEdits]);
        reset();
    }

    /// Maps to: CC `:60-83` — every slot is nullable; clearing it stops
    /// delivery.
    #[test]
    fn clearing_a_listener_stops_delivery() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset();
        let (listener, seen) = recording_metadata_listener();
        set_session_metadata_changed_listener(Some(listener));
        set_session_metadata_changed_listener(None);

        notify_session_state_changed(SessionState::Idle, None);

        assert!(seen.lock().unwrap().is_empty());
        reset();
    }
}
