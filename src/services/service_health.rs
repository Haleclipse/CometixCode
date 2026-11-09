//! Shared service-health state machine (README §1.9, retrieval 3.2).
//!
//! @nyan: every self-hosted service (embedding, rerank, Qdrant, SearXNG) and
//! the Decision Provider backend shares one `Ready ⇄ Down(until)` machine.
//! The point is to fail *fast*: a powered-off corp-pc does not refuse a
//! connection, it hangs until a timeout. After the first failure the service
//! is marked Down for a backoff window and every caller skips it without
//! opening a socket, so a query costs one connect timeout at most, not one
//! per query.
//!
//! Behaviour (README §1.9):
//! - `Ready ⇄ Down(until)`;
//! - connect timeout and request timeout are separate (callers time the
//!   connect phase and the request phase differently);
//! - backoff starts at 30 s and doubles to a 5 min cap;
//! - while Down, no request is made;
//! - when the window expires, the *next use* starts one background probe; no
//!   timer polls the service and nothing probes at startup;
//! - one instance per service, shared process-wide.
//!
//! The machine owns no HTTP client: the probe is a closure supplied by the
//! owning module, so this file stays testable offline.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Services tracked by the shared health machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    Embedding,
    Rerank,
    Qdrant,
    Searxng,
    Emrank,
}

impl ServiceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceKind::Embedding => "embedding",
            ServiceKind::Rerank => "rerank",
            ServiceKind::Qdrant => "qdrant",
            ServiceKind::Searxng => "searxng",
            ServiceKind::Emrank => "emrank",
        }
    }

    /// Per-service timeouts: the connect phase covers a hanging host (a
    /// powered-off corp-pc, a DERP-relayed tailnet) and is deliberately
    /// shorter than the request deadline.
    pub fn default_timeouts(self) -> (Duration, Duration) {
        match self {
            // retrieval 3.2: connect 1.5 s covers a DERP-relayed handshake;
            // query embedding 2.5 s, sync batches override the request timeout.
            ServiceKind::Embedding => (Duration::from_millis(1_500), Duration::from_millis(2_500)),
            // retrieval 3.2 / 4.9.
            ServiceKind::Rerank => (Duration::from_millis(1_500), Duration::from_millis(2_000)),
            // retrieval 3.2.
            ServiceKind::Qdrant => (Duration::from_millis(1_500), Duration::from_millis(1_500)),
            // external search 3.2.1: per-provider deadline is 8 s.
            ServiceKind::Searxng => (Duration::from_millis(1_500), Duration::from_millis(8_000)),
            // decision-provider 5.2: the critical-path budget is 800 ms; the
            // request timeout is the background budget.
            ServiceKind::Emrank => (Duration::from_millis(1_000), Duration::from_millis(2_000)),
        }
    }
}

/// Why a service went Down. Kept only for rendering and logging; every kind
/// produces the same state transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// Connection refused, connect/handshake timeout, DNS failure.
    Unreachable,
    /// Request timeout after the connection was established.
    Timeout,
    /// HTTP 429.
    RateLimited,
    /// HTTP 401 / 403.
    Auth,
    /// HTTP 402 / quota exhausted.
    Budget,
    /// The service rejected the configured model id (unknown / not enabled).
    /// The endpoint is healthy; the configuration is not.
    ModelNotFound,
    /// Malformed or out-of-range response.
    InvalidResponse,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::Unreachable => "unreachable",
            FailureKind::Timeout => "timeout",
            FailureKind::RateLimited => "rate_limited",
            FailureKind::Auth => "auth_failed",
            FailureKind::Budget => "budget_exhausted",
            FailureKind::ModelNotFound => "model_not_found",
            FailureKind::InvalidResponse => "invalid_response",
        }
    }
}

/// First backoff window; doubles per consecutive failure.
pub const BACKOFF_BASE: Duration = Duration::from_secs(30);
/// Ceiling for the exponential backoff.
pub const BACKOFF_CAP: Duration = Duration::from_secs(300);

/// Backoff for the Nth consecutive failure (1-based): 30 s, 60 s, 120 s,
/// 240 s, 300 s, 300 s, …
pub fn backoff_for(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(16);
    let seconds = BACKOFF_BASE
        .as_secs()
        .saturating_mul(1u64 << exponent)
        .min(BACKOFF_CAP.as_secs());
    Duration::from_secs(seconds)
}

/// The caller's permission to use the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPermit {
    /// Ready: make the request.
    Allowed,
    /// The backoff window expired. One background probe has been claimed;
    /// make the request as usual if you are the probe owner, otherwise skip.
    Probe,
    /// Down: skip without opening a socket.
    Skipped,
}

/// Probe outcome. `Err` is any failure; the machine only needs yes/no.
pub type ProbeResult = Result<(), ()>;
pub type ProbeFn = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ProbeResult> + Send>>
        + Send
        + Sync,
>;

struct State {
    /// `None` = Ready; `Some(until)` = Down until that instant.
    down_until: Option<Instant>,
    consecutive_failures: u32,
    last_failure: Option<FailureKind>,
    /// A probe was claimed and has not reported back yet.
    probing: bool,
}

pub struct ServiceHealth {
    kind: ServiceKind,
    connect_timeout: Duration,
    request_timeout: Duration,
    probe: Mutex<Option<ProbeFn>>,
    state: Mutex<State>,
}

impl std::fmt::Debug for ServiceHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        f.debug_struct("ServiceHealth")
            .field("kind", &self.kind)
            .field("down_until", &state.down_until)
            .field("consecutive_failures", &state.consecutive_failures)
            .field("probing", &state.probing)
            .finish()
    }
}

impl ServiceHealth {
    pub fn new(kind: ServiceKind) -> Self {
        let (connect_timeout, request_timeout) = kind.default_timeouts();
        Self::with_timeouts(kind, connect_timeout, request_timeout)
    }

    pub fn with_timeouts(
        kind: ServiceKind,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Self {
        Self {
            kind,
            connect_timeout,
            request_timeout,
            probe: Mutex::new(None),
            state: Mutex::new(State {
                down_until: None,
                consecutive_failures: 0,
                last_failure: None,
                probing: false,
            }),
        }
    }

    pub fn kind(&self) -> ServiceKind {
        self.kind
    }

    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Install the probe closure. `None` clears it.
    pub fn set_probe(&self, probe: Option<ProbeFn>) {
        *self.probe.lock().unwrap_or_else(|p| p.into_inner()) = probe;
    }

    pub fn is_ready(&self) -> bool {
        self.call_permit_at(Instant::now()) == CallPermit::Allowed
    }

    pub fn is_down(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .down_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub fn last_failure(&self) -> Option<FailureKind> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last_failure
    }

    /// Ask whether a call may proceed. On an expired backoff window this
    /// returns [`CallPermit::Probe`] exactly once and marks a probe in flight;
    /// the caller must then call [`Self::spawn_probe`].
    pub fn call_permit(&self) -> CallPermit {
        self.call_permit_at(Instant::now())
    }

    pub(crate) fn call_permit_at(&self, now: Instant) -> CallPermit {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match state.down_until {
            None => CallPermit::Allowed,
            Some(until) if now < until => CallPermit::Skipped,
            Some(_) if state.probing => CallPermit::Skipped,
            Some(_) => {
                state.probing = true;
                CallPermit::Probe
            }
        }
    }

    /// Start the background probe claimed by [`CallPermit::Probe`]. Without a
    /// probe closure the service is restored to Ready so an unconfigured
    /// instance cannot stay Down forever.
    ///
    /// The probe must outlive the turn that triggered it
    /// (`utils::process_runtime` documents why a per-query runtime is the wrong
    /// home). When no process-lifetime runtime is published — tests and
    /// embedded callers — the ambient runtime is used; with neither, the probe
    /// cannot run and the service stays Down instead of wedging `probing` on.
    pub fn spawn_probe(self: &Arc<Self>) {
        let probe = self.probe.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let Some(probe) = probe else {
            self.finish_probe(true);
            return;
        };
        let health = Arc::clone(self);
        let request_timeout = self.request_timeout;
        let future = async move {
            let ok = matches!(
                tokio::time::timeout(request_timeout, probe()).await,
                Ok(Ok(()))
            );
            health.finish_probe(ok);
        };
        if let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
            handle.spawn(future);
        } else if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(future);
        } else {
            // Cannot probe: release the claim and keep the service Down, so a
            // later caller (on a runtime) can try again.
            self.release_probe_claim();
        }
    }

    fn release_probe_claim(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.probing = false;
    }

    /// Record a successful request: back to Ready and reset the backoff.
    pub fn record_success(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.down_until = None;
        state.consecutive_failures = 0;
        state.last_failure = None;
        state.probing = false;
    }

    /// Record a failed request: Down for the next backoff window.
    pub fn record_failure(&self, kind: FailureKind) {
        self.record_failure_at(Instant::now(), kind);
    }

    pub(crate) fn record_failure_at(&self, now: Instant, kind: FailureKind) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.last_failure = Some(kind);
        state.down_until = Some(now + backoff_for(state.consecutive_failures));
        state.probing = false;
    }

    /// Report the outcome of a probe. Success restores Ready; failure keeps
    /// the service Down with the next (doubled) backoff window.
    pub fn finish_probe(&self, healthy: bool) {
        if healthy {
            self.record_success();
        } else {
            self.record_failure(FailureKind::Unreachable);
        }
    }

    /// Test/administrative reset: Ready, no failure history.
    pub fn reset(&self) {
        self.record_success();
    }
}

// ════════════════════════════════════════════════════════════
// Process-wide registry
// ════════════════════════════════════════════════════════════

struct Registry {
    embedding: Arc<ServiceHealth>,
    rerank: Arc<ServiceHealth>,
    qdrant: Arc<ServiceHealth>,
    searxng: Arc<ServiceHealth>,
    emrank: Arc<ServiceHealth>,
}

fn registry() -> &'static Registry {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Registry {
        embedding: Arc::new(ServiceHealth::new(ServiceKind::Embedding)),
        rerank: Arc::new(ServiceHealth::new(ServiceKind::Rerank)),
        qdrant: Arc::new(ServiceHealth::new(ServiceKind::Qdrant)),
        searxng: Arc::new(ServiceHealth::new(ServiceKind::Searxng)),
        emrank: Arc::new(ServiceHealth::new(ServiceKind::Emrank)),
    })
}

/// The shared instance for a service. Callers must not construct their own:
/// retrieval, external search and the decision provider all need to see the
/// same Down window, or a dead corp-pc would be paid for once per module.
pub fn shared(kind: ServiceKind) -> Arc<ServiceHealth> {
    match kind {
        ServiceKind::Embedding => Arc::clone(&registry().embedding),
        ServiceKind::Rerank => Arc::clone(&registry().rerank),
        ServiceKind::Qdrant => Arc::clone(&registry().qdrant),
        ServiceKind::Searxng => Arc::clone(&registry().searxng),
        ServiceKind::Emrank => Arc::clone(&registry().emrank),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn backoff_starts_at_thirty_seconds_doubles_and_caps_at_five_minutes() {
        assert_eq!(backoff_for(1), Duration::from_secs(30));
        assert_eq!(backoff_for(2), Duration::from_secs(60));
        assert_eq!(backoff_for(3), Duration::from_secs(120));
        assert_eq!(backoff_for(4), Duration::from_secs(240));
        assert_eq!(backoff_for(5), Duration::from_secs(300));
        assert_eq!(backoff_for(6), BACKOFF_CAP);
        assert_eq!(backoff_for(1_000), BACKOFF_CAP);
    }

    #[test]
    fn a_fresh_service_allows_requests_and_never_probes_on_construction() {
        let health = ServiceHealth::new(ServiceKind::Qdrant);
        assert_eq!(health.call_permit(), CallPermit::Allowed);
        assert!(health.is_ready());
        assert!(!health.is_down());
        assert!(health.last_failure().is_none());
    }

    #[test]
    fn a_failure_marks_down_and_skips_requests_without_reaching_the_service() {
        let health = ServiceHealth::new(ServiceKind::Embedding);
        let t0 = Instant::now();
        health.record_failure_at(t0, FailureKind::Unreachable);

        assert_eq!(health.call_permit_at(at(t0, 1)), CallPermit::Skipped);
        assert_eq!(health.call_permit_at(at(t0, 29_999)), CallPermit::Skipped);
        assert_eq!(health.last_failure(), Some(FailureKind::Unreachable));
    }

    #[test]
    fn the_expired_window_yields_exactly_one_probe_permit() {
        let health = ServiceHealth::new(ServiceKind::Rerank);
        let t0 = Instant::now();
        health.record_failure_at(t0, FailureKind::Timeout);

        assert_eq!(health.call_permit_at(t0 + BACKOFF_BASE), CallPermit::Probe);
        // A second caller during the probe must not start another one.
        assert_eq!(
            health.call_permit_at(t0 + BACKOFF_BASE + Duration::from_millis(1)),
            CallPermit::Skipped
        );
    }

    #[test]
    fn probe_success_restores_ready_and_resets_the_backoff() {
        let health = ServiceHealth::new(ServiceKind::Rerank);
        let t0 = Instant::now();
        health.record_failure_at(t0, FailureKind::Unreachable);
        assert_eq!(health.call_permit_at(t0 + BACKOFF_BASE), CallPermit::Probe);

        health.finish_probe(true);
        assert_eq!(health.call_permit(), CallPermit::Allowed);
        assert!(health.last_failure().is_none());

        // The next failure starts again at 30 s, not where it left off.
        let t1 = Instant::now();
        health.record_failure_at(t1, FailureKind::Unreachable);
        assert_eq!(health.call_permit_at(at(t1, 29_999)), CallPermit::Skipped);
        assert_eq!(health.call_permit_at(t1 + BACKOFF_BASE), CallPermit::Probe);
    }

    #[test]
    fn probe_failure_doubles_the_next_window() {
        let health = ServiceHealth::new(ServiceKind::Searxng);
        let t0 = Instant::now();
        health.record_failure_at(t0, FailureKind::Unreachable);
        assert_eq!(health.call_permit_at(t0 + BACKOFF_BASE), CallPermit::Probe);

        let t1 = t0 + BACKOFF_BASE;
        health.record_failure_at(t1, FailureKind::Unreachable);
        assert_eq!(
            health.call_permit_at(t1 + Duration::from_secs(59)),
            CallPermit::Skipped
        );
        assert_eq!(
            health.call_permit_at(t1 + Duration::from_secs(60)),
            CallPermit::Probe
        );
    }

    #[test]
    fn consecutive_failures_saturate_at_the_cap() {
        let health = ServiceHealth::new(ServiceKind::Emrank);
        let mut now = Instant::now();
        for _ in 0..20 {
            health.record_failure_at(now, FailureKind::RateLimited);
            now += BACKOFF_CAP;
        }
        health.record_failure_at(now, FailureKind::RateLimited);
        // After the cap every window is exactly 5 minutes.
        assert_eq!(
            health.call_permit_at(now + Duration::from_secs(299)),
            CallPermit::Skipped
        );
        assert_eq!(health.call_permit_at(now + BACKOFF_CAP), CallPermit::Probe);
    }

    #[test]
    fn timeouts_are_per_service_and_split_connect_from_request() {
        let embedding = ServiceHealth::new(ServiceKind::Embedding);
        assert_eq!(embedding.connect_timeout(), Duration::from_millis(1_500));
        assert_eq!(embedding.request_timeout(), Duration::from_millis(2_500));

        let qdrant = ServiceHealth::new(ServiceKind::Qdrant);
        assert_eq!(qdrant.connect_timeout(), Duration::from_millis(1_500));
        assert_eq!(qdrant.request_timeout(), Duration::from_millis(1_500));

        let rerank = ServiceHealth::new(ServiceKind::Rerank);
        assert_eq!(rerank.request_timeout(), Duration::from_millis(2_000));

        let searxng = ServiceHealth::new(ServiceKind::Searxng);
        assert_eq!(searxng.request_timeout(), Duration::from_millis(8_000));

        let emrank = ServiceHealth::new(ServiceKind::Emrank);
        assert_eq!(emrank.request_timeout(), Duration::from_millis(2_000));
        // Every service's connect budget is shorter than its request budget.
        for kind in [
            ServiceKind::Embedding,
            ServiceKind::Rerank,
            ServiceKind::Qdrant,
            ServiceKind::Searxng,
            ServiceKind::Emrank,
        ] {
            let (connect, request) = kind.default_timeouts();
            assert!(connect <= request, "{kind:?}");
        }
    }

    #[test]
    fn the_registry_hands_out_one_instance_per_service() {
        let a = shared(ServiceKind::Embedding);
        let b = shared(ServiceKind::Embedding);
        assert!(Arc::ptr_eq(&a, &b));

        let rerank = shared(ServiceKind::Rerank);
        assert!(!Arc::ptr_eq(&a, &rerank));

        // Restore global state for other tests in this process.
        for kind in [
            ServiceKind::Embedding,
            ServiceKind::Rerank,
            ServiceKind::Qdrant,
            ServiceKind::Searxng,
            ServiceKind::Emrank,
        ] {
            shared(kind).reset();
        }
    }

    #[tokio::test]
    async fn an_expired_window_spawns_one_background_probe_and_restores_the_service() {
        let health = Arc::new(ServiceHealth::with_timeouts(
            ServiceKind::Rerank,
            Duration::from_millis(5),
            Duration::from_millis(50),
        ));
        let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&probes);
        health.set_probe(Some(Arc::new(move || {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        })));

        let t0 = Instant::now();
        health.record_failure_at(t0, FailureKind::Unreachable);
        assert_eq!(health.call_permit_at(t0 + BACKOFF_BASE), CallPermit::Probe);
        health.spawn_probe();

        for _ in 0..100 {
            if health.is_ready() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(health.is_ready(), "probe success restores Ready");
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_probe_that_times_out_keeps_the_service_down() {
        let health = Arc::new(ServiceHealth::with_timeouts(
            ServiceKind::Embedding,
            Duration::from_millis(5),
            Duration::from_millis(20),
        ));
        health.set_probe(Some(Arc::new(|| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            })
        })));

        let t0 = Instant::now();
        health.record_failure_at(t0, FailureKind::Unreachable);
        assert_eq!(health.call_permit_at(t0 + BACKOFF_BASE), CallPermit::Probe);
        health.spawn_probe();

        // The probe hangs past the request timeout, so the machine records
        // another failure (not success) and stays Down.
        for _ in 0..100 {
            if health.last_failure().is_some() && !health.is_ready() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!health.is_ready());
        assert_eq!(health.last_failure(), Some(FailureKind::Unreachable));
    }

    #[tokio::test]
    async fn an_unconfigured_probe_restores_ready_instead_of_staying_down_forever() {
        let health = Arc::new(ServiceHealth::new(ServiceKind::Qdrant));
        let t0 = Instant::now();
        health.record_failure_at(t0, FailureKind::Unreachable);
        assert_eq!(health.call_permit_at(t0 + BACKOFF_BASE), CallPermit::Probe);
        health.spawn_probe();
        assert_eq!(health.call_permit(), CallPermit::Allowed);
    }
}
