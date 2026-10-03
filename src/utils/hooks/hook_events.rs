//! Hook execution event bus.
//! Maps to: CC `utils/hooks/hookEvents.ts`.
//!
//! This module is intentionally separate from the shell hook execution modules:
//! it broadcasts hook lifecycle events for SDK/print-mode observers and buffers
//! early events until a handler is registered. It does not decide which hooks
//! match or execute hook commands.

use crate::services::hooks::{HOOK_EVENTS, HookEvent};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Maps to: CC `ALWAYS_EMITTED_HOOK_EVENTS`.
const ALWAYS_EMITTED_HOOK_EVENTS: &[&str] = &["SessionStart", "Setup"];
/// Maps to: CC `MAX_PENDING_EVENTS`.
pub const MAX_PENDING_HOOK_EVENTS: usize = 100;

/// Maps to: CC `HookStartedEvent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookStartedEvent {
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
}

/// Maps to: CC `HookProgressEvent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookProgressEvent {
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    pub stdout: String,
    pub stderr: String,
    pub output: String,
}

/// Maps to: CC `HookResponseEvent.outcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookResponseOutcome {
    Success,
    Error,
    Cancelled,
}

impl HookResponseOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Maps to: CC `HookResponseEvent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookResponseEvent {
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    pub output: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub outcome: HookResponseOutcome,
}

/// Maps to: CC `HookExecutionEvent` discriminated union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookExecutionEvent {
    Started(HookStartedEvent),
    Progress(HookProgressEvent),
    Response(HookResponseEvent),
}

impl HookExecutionEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Started(_) => "started",
            Self::Progress(_) => "progress",
            Self::Response(_) => "response",
        }
    }
}

/// Maps to: CC `HookEventHandler`.
pub type HookEventHandler = Arc<dyn Fn(HookExecutionEvent) + Send + Sync + 'static>;

#[derive(Default)]
struct HookEventState {
    pending_events: Vec<HookExecutionEvent>,
    event_handler: Option<HookEventHandler>,
    all_hook_events_enabled: bool,
}

static HOOK_EVENT_STATE: LazyLock<Mutex<HookEventState>> =
    LazyLock::new(|| Mutex::new(HookEventState::default()));

fn hook_event_name(event: &HookExecutionEvent) -> &str {
    match event {
        HookExecutionEvent::Started(event) => &event.hook_event,
        HookExecutionEvent::Progress(event) => &event.hook_event,
        HookExecutionEvent::Response(event) => &event.hook_event,
    }
}

fn is_known_hook_event(hook_event: &str) -> bool {
    HOOK_EVENTS
        .iter()
        .map(|event| event.as_str())
        .any(|known| known == hook_event)
}

/// Maps to: CC `shouldEmit(hookEvent)`.
pub fn should_emit_hook_event(hook_event: &str) -> bool {
    if ALWAYS_EMITTED_HOOK_EVENTS.contains(&hook_event) {
        return true;
    }
    HOOK_EVENT_STATE
        .lock()
        .expect("hook event state poisoned")
        .all_hook_events_enabled
        && is_known_hook_event(hook_event)
}

fn emit(event: HookExecutionEvent) {
    if !should_emit_hook_event(hook_event_name(&event)) {
        return;
    }

    let handler = {
        let mut state = HOOK_EVENT_STATE.lock().expect("hook event state poisoned");
        if let Some(handler) = state.event_handler.clone() {
            Some(handler)
        } else {
            state.pending_events.push(event.clone());
            if state.pending_events.len() > MAX_PENDING_HOOK_EVENTS {
                let overflow = state.pending_events.len() - MAX_PENDING_HOOK_EVENTS;
                state.pending_events.drain(0..overflow);
            }
            None
        }
    };

    if let Some(handler) = handler {
        handler(event);
    }
}

/// Maps to: CC `registerHookEventHandler(handler)`.
pub fn register_hook_event_handler(handler: Option<HookEventHandler>) {
    let pending = {
        let mut state = HOOK_EVENT_STATE.lock().expect("hook event state poisoned");
        state.event_handler = handler.clone();
        if handler.is_some() {
            state.pending_events.drain(..).collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    };

    if let Some(handler) = handler {
        for event in pending {
            handler(event);
        }
    }
}

/// Maps to: CC `emitHookStarted(...)`.
pub fn emit_hook_started(
    hook_id: impl Into<String>,
    hook_name: impl Into<String>,
    hook_event: impl Into<String>,
) {
    emit(HookExecutionEvent::Started(HookStartedEvent {
        hook_id: hook_id.into(),
        hook_name: hook_name.into(),
        hook_event: hook_event.into(),
    }));
}

/// Maps to: CC `emitHookProgress(...)`.
pub fn emit_hook_progress(event: HookProgressEvent) {
    emit(HookExecutionEvent::Progress(event));
}

/// Maps to: CC `emitHookResponse(...)`.
pub fn emit_hook_response(event: HookResponseEvent) {
    // CC always writes full hook output to debug logs here. Cometix keeps that
    // observability boundary for future tracing integration but does not emit
    // telemetry/analytics from this module.
    emit(HookExecutionEvent::Response(event));
}

/// Maps to: CC `setAllHookEventsEnabled(enabled)`.
pub fn set_all_hook_events_enabled(enabled: bool) {
    HOOK_EVENT_STATE
        .lock()
        .expect("hook event state poisoned")
        .all_hook_events_enabled = enabled;
}

/// Maps to: CC `clearHookEventState()`.
pub fn clear_hook_event_state() {
    let mut state = HOOK_EVENT_STATE.lock().expect("hook event state poisoned");
    state.event_handler = None;
    state.pending_events.clear();
    state.all_hook_events_enabled = false;
}

/// Synchronous output snapshot for progress polling.
/// Maps to the object returned by CC `startHookProgressInterval.getOutput()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookProgressOutput {
    pub stdout: String,
    pub stderr: String,
    pub output: String,
}

/// Parameters for `start_hook_progress_interval`.
/// Maps to: CC `startHookProgressInterval(params)`.
pub struct HookProgressIntervalParams {
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    pub get_output: Arc<dyn Fn() -> HookProgressOutput + Send + Sync + 'static>,
    pub interval_ms: Option<u64>,
}

/// Stop handle returned by `start_hook_progress_interval`.
pub struct HookProgressInterval {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl HookProgressInterval {
    pub fn noop() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            handle: None,
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for HookProgressInterval {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Maps to: CC `startHookProgressInterval(...)`.
pub fn start_hook_progress_interval(params: HookProgressIntervalParams) -> HookProgressInterval {
    if !should_emit_hook_event(&params.hook_event) {
        return HookProgressInterval::noop();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let interval = Duration::from_millis(params.interval_ms.unwrap_or(1_000));
    let handle = std::thread::spawn(move || {
        let mut last_emitted_output = String::new();
        while !stop_for_thread.load(Ordering::SeqCst) {
            std::thread::sleep(interval);
            if stop_for_thread.load(Ordering::SeqCst) {
                break;
            }
            let snapshot = (params.get_output)();
            if snapshot.output == last_emitted_output {
                continue;
            }
            last_emitted_output = snapshot.output.clone();
            emit_hook_progress(HookProgressEvent {
                hook_id: params.hook_id.clone(),
                hook_name: params.hook_name.clone(),
                hook_event: params.hook_event.clone(),
                stdout: snapshot.stdout,
                stderr: snapshot.stderr,
                output: snapshot.output,
            });
        }
    });

    HookProgressInterval {
        stop,
        handle: Some(handle),
    }
}

/// Helper for APIs that traffic in the Rust hook enum.
pub fn hook_event_name_from_enum(event: HookEvent) -> &'static str {
    event.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TestStateLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

    fn collect_handler(into: Arc<Mutex<Vec<HookExecutionEvent>>>) -> HookEventHandler {
        Arc::new(move |event| into.lock().expect("events mutex").push(event))
    }

    #[test]
    fn always_emitted_events_buffer_and_drain_without_all_events_enabled() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_hook_event_state();
        emit_hook_started("1", "startup", "SessionStart");
        emit_hook_started("2", "pre", "PreToolUse");

        let events = Arc::new(Mutex::new(Vec::new()));
        register_hook_event_handler(Some(collect_handler(Arc::clone(&events))));

        let events = events.lock().expect("events mutex");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind(), "started");
        assert!(matches!(
            &events[0],
            HookExecutionEvent::Started(HookStartedEvent { hook_event, .. }) if hook_event == "SessionStart"
        ));
        clear_hook_event_state();
    }

    #[test]
    fn all_events_enabled_emits_known_hook_events_but_not_statusline() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_hook_event_state();
        let events = Arc::new(Mutex::new(Vec::new()));
        register_hook_event_handler(Some(collect_handler(Arc::clone(&events))));
        set_all_hook_events_enabled(true);

        emit_hook_started("1", "pre", "PreToolUse");
        emit_hook_started("2", "status", "StatusLine");

        let events = events.lock().expect("events mutex");
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            HookExecutionEvent::Started(HookStartedEvent { hook_event, .. }) if hook_event == "PreToolUse"
        ));
        clear_hook_event_state();
    }

    #[test]
    fn pending_events_are_capped_like_official_ring_buffer() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_hook_event_state();
        for index in 0..(MAX_PENDING_HOOK_EVENTS + 5) {
            emit_hook_started(index.to_string(), "setup", "Setup");
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        register_hook_event_handler(Some(collect_handler(Arc::clone(&events))));

        let events = events.lock().expect("events mutex");
        assert_eq!(events.len(), MAX_PENDING_HOOK_EVENTS);
        assert!(matches!(
            &events[0],
            HookExecutionEvent::Started(HookStartedEvent { hook_id, .. }) if hook_id == "5"
        ));
        clear_hook_event_state();
    }

    #[test]
    fn progress_interval_emits_only_changed_output() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_hook_event_state();
        set_all_hook_events_enabled(true);
        let events = Arc::new(Mutex::new(Vec::new()));
        register_hook_event_handler(Some(collect_handler(Arc::clone(&events))));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_provider = Arc::clone(&calls);

        let interval = start_hook_progress_interval(HookProgressIntervalParams {
            hook_id: "hook-1".to_string(),
            hook_name: "pre".to_string(),
            hook_event: "PreToolUse".to_string(),
            interval_ms: Some(10),
            get_output: Arc::new(move || {
                let call = calls_for_provider.fetch_add(1, Ordering::SeqCst);
                let output = if call < 2 { "same" } else { "changed" };
                HookProgressOutput {
                    stdout: output.to_string(),
                    stderr: String::new(),
                    output: output.to_string(),
                }
            }),
        });

        std::thread::sleep(Duration::from_millis(55));
        interval.stop();

        let progress = events
            .lock()
            .expect("events mutex")
            .iter()
            .filter(|event| matches!(event, HookExecutionEvent::Progress(_)))
            .cloned()
            .collect::<Vec<_>>();
        assert!(progress.len() >= 2, "progress={progress:?}");
        assert!(matches!(
            &progress[0],
            HookExecutionEvent::Progress(HookProgressEvent { output, .. }) if output == "same"
        ));
        assert!(progress.iter().any(|event| matches!(
            event,
            HookExecutionEvent::Progress(HookProgressEvent { output, .. }) if output == "changed"
        )));
        clear_hook_event_state();
    }
}
