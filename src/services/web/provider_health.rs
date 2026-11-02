//! Commercial web search and fetch provider health state machine.
//!
//! @nyan: six commercial providers (anysearch, brave, deepseek, exa, firecrawl,
//! tavily) share a `(provider, capability)`-keyed health machine. SearXNG is
//! self-hosted and continues to use [`crate::services::service_health::ServiceHealth`].
//!
//! Key differences from self-hosted service health:
//! 1. Failure grading: soft errors (timeout, network, 5xx, invalid) enter Down on 2 consecutive
//!    failures; rate limits (429) enter Down on 1st failure; auth (401, no key) and quota
//!    exhausted (402, 432, Brave monthly 0) are hard errors entering Down on 1st failure.
//! 2. Error mapping: this module defines its own mapping from [`ProviderError`] to soft/hard
//!    failures and deliberately does not reuse `client::failure_kind` (where `QuotaExhausted`
//!    was mapped to `Budget`, whereas here it is a hard failure).
//! 3. Keyed by `(provider, capability)`: Search and Fetch are tracked independently. Auth and
//!    QuotaExhausted are account-level and write to both capabilities.
//! 4. Live call probing: commercial APIs are paid, so no synthetic background probe is sent;
//!    instead, the first real call after the backoff window expires acts as the probe.
//! 5. Credential fingerprint: `sha256(base_url || key)[..16]` detects key changes and resets
//!    cooling down immediately without waiting for the backoff window.
//! 6. RAII guard: permits are guards whose drops (due to cancellation or panic) release the
//!    probing state without counting success or failure.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::providers::ProviderError;

/// The six commercial providers tracked by this health registry.
pub const COMMERCIAL_PROVIDERS: [&str; 6] = [
    "anysearch",
    "brave",
    "deepseek",
    "exa",
    "firecrawl",
    "tavily",
];

pub fn is_commercial_provider(name: &str) -> bool {
    COMMERCIAL_PROVIDERS.contains(&name)
}

/// Provider capabilities tracked separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    Search,
    Fetch,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Fetch => "fetch",
        }
    }
}

/// Failure severity level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureSeverity {
    Soft,
    Hard,
}

/// Action to take on a provider error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureAction {
    Counted {
        severity: FailureSeverity,
        threshold: u32,
        is_account_level: bool,
    },
    Ignored,
}

/// Classify a provider error for a given capability.
///
/// Note: this module defines its own mapping rather than reusing `client::failure_kind`,
/// because `QuotaExhausted` is categorized as a Hard failure here (with 10-60 min backoff),
/// not a budget exhaustion.
pub fn classify_failure(capability: Capability, error: &ProviderError) -> FailureAction {
    match error {
        ProviderError::Timeout | ProviderError::Network | ProviderError::Invalid => {
            FailureAction::Counted {
                severity: FailureSeverity::Soft,
                threshold: 2,
                is_account_level: false,
            }
        }
        ProviderError::RateLimited => FailureAction::Counted {
            severity: FailureSeverity::Soft,
            threshold: 1,
            is_account_level: false,
        },
        ProviderError::Auth => FailureAction::Counted {
            severity: FailureSeverity::Hard,
            threshold: 1,
            is_account_level: true,
        },
        ProviderError::QuotaExhausted => FailureAction::Counted {
            severity: FailureSeverity::Hard,
            threshold: 1,
            is_account_level: true,
        },
        ProviderError::Http(status) => match *status {
            401 => FailureAction::Counted {
                severity: FailureSeverity::Hard,
                threshold: 1,
                is_account_level: true,
            },
            402 | 432 => FailureAction::Counted {
                severity: FailureSeverity::Hard,
                threshold: 1,
                is_account_level: true,
            },
            403 => {
                if capability == Capability::Search {
                    FailureAction::Counted {
                        severity: FailureSeverity::Hard,
                        threshold: 1,
                        is_account_level: true,
                    }
                } else {
                    // Fetch 403 indicates page blocked / forbidden, not an account key problem.
                    FailureAction::Ignored
                }
            }
            429 => FailureAction::Counted {
                severity: FailureSeverity::Soft,
                threshold: 1,
                is_account_level: false,
            },
            500..=599 => FailureAction::Counted {
                severity: FailureSeverity::Soft,
                threshold: 2,
                is_account_level: false,
            },
            // Other 4xx (e.g. 400 Bad Request, 404 Not Found) are request issues, not provider outages.
            _ => FailureAction::Ignored,
        },
        ProviderError::BudgetExhausted
        | ProviderError::Unsupported
        | ProviderError::Unavailable => FailureAction::Ignored,
    }
}

/// Hard failure backoff: 10 min base, doubles up to 60 min ceiling.
pub const HARD_BACKOFF_BASE: Duration = Duration::from_secs(600);
pub const HARD_BACKOFF_CAP: Duration = Duration::from_secs(3600);

pub fn hard_backoff_for(downs: u32) -> Duration {
    let exponent = downs.saturating_sub(1).min(16);
    let seconds = HARD_BACKOFF_BASE
        .as_secs()
        .saturating_mul(1u64 << exponent)
        .min(HARD_BACKOFF_CAP.as_secs());
    Duration::from_secs(seconds)
}

/// Computes credential fingerprint = sha256(base_url || key)[..16].
///
/// Only the hex digest prefix is stored and exposed, never the secret key.
pub fn compute_fingerprint(base_url: &str, api_key: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(base_url.as_bytes());
    if let Some(key) = api_key {
        hasher.update(key.as_bytes());
    }
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

#[derive(Clone, Debug, Default)]
struct EntryState {
    soft_streak: u32,
    downs: u32,
    down_until: Option<Instant>,
    fingerprint: Option<String>,
    probing: bool,
    last_reason: Option<String>,
    last_severity: Option<FailureSeverity>,
}

impl EntryState {
    fn is_cooling_down_at(&self, now: Instant) -> bool {
        self.down_until.is_some_and(|until| now < until)
    }

    fn is_soft_down_at(&self, now: Instant) -> bool {
        self.is_cooling_down_at(now) && self.last_severity == Some(FailureSeverity::Soft)
    }

    fn is_hard_down_at(&self, now: Instant) -> bool {
        self.is_cooling_down_at(now) && self.last_severity == Some(FailureSeverity::Hard)
    }

    fn clear_down(&mut self) {
        self.soft_streak = 0;
        self.downs = 0;
        self.down_until = None;
        self.probing = false;
        self.last_reason = None;
        self.last_severity = None;
    }
}

/// Outcome of attempting to acquire a call permit.
#[derive(Debug)]
pub enum PermitOutcome {
    /// Provider is Ready: proceed with call.
    Allowed(HealthGuard),
    /// Provider was Down and its window expired: caller is granted the single probe slot.
    Probe(HealthGuard),
    /// Provider is in Down backoff or another caller is currently probing: skip call.
    Skipped {
        reason: String,
        severity: FailureSeverity,
    },
}

/// RAII permit guard.
///
/// If dropped without calling [`HealthGuard::record_success`] or [`HealthGuard::record_failure`],
/// any probe reservation is released without counting a success or failure (e.g. on cancellation/panic).
#[derive(Debug)]
pub struct HealthGuard {
    registry: Arc<ProviderHealthRegistry>,
    provider: String,
    capability: Capability,
    is_probe: bool,
    recorded: bool,
}

impl HealthGuard {
    pub fn is_probe(&self) -> bool {
        self.is_probe
    }

    pub fn record_success(mut self) {
        self.recorded = true;
        self.registry
            .record_success_at(&self.provider, self.capability, Instant::now());
    }

    pub fn record_success_at(mut self, now: Instant) {
        self.recorded = true;
        self.registry
            .record_success_at(&self.provider, self.capability, now);
    }

    pub fn record_failure(mut self, error: &ProviderError) {
        self.recorded = true;
        self.registry
            .record_failure_at(&self.provider, self.capability, error, Instant::now());
    }

    pub fn record_failure_at(mut self, error: &ProviderError, now: Instant) {
        self.recorded = true;
        self.registry
            .record_failure_at(&self.provider, self.capability, error, now);
    }
}

impl Drop for HealthGuard {
    fn drop(&mut self) {
        if !self.recorded && self.is_probe {
            self.registry.release_probe(&self.provider, self.capability);
        }
    }
}

#[derive(Debug)]
pub struct ProviderHealthRegistry {
    entries: Mutex<HashMap<(String, Capability), EntryState>>,
}

impl ProviderHealthRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
        })
    }

    pub fn acquire_permit_opt_release(
        self: &Arc<Self>,
        provider: &str,
        capability: Capability,
        fingerprint: Option<&str>,
        is_released: bool,
        now: Instant,
    ) -> PermitOutcome {
        if !is_commercial_provider(provider) {
            return PermitOutcome::Allowed(HealthGuard {
                registry: Arc::clone(self),
                provider: provider.to_string(),
                capability,
                is_probe: false,
                recorded: false,
            });
        }

        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());

        // Check credential fingerprint: if changed, reset Down state across ALL capabilities of this provider.
        if let Some(fp) = fingerprint {
            let mut mismatch = false;
            for cap in [Capability::Search, Capability::Fetch] {
                if let Some(entry) = entries.get(&(provider.to_string(), cap)) {
                    if let Some(existing_fp) = &entry.fingerprint {
                        if existing_fp != fp {
                            mismatch = true;
                            break;
                        }
                    }
                }
            }
            if mismatch {
                for cap in [Capability::Search, Capability::Fetch] {
                    let entry = entries.entry((provider.to_string(), cap)).or_default();
                    entry.clear_down();
                    entry.fingerprint = Some(fp.to_string());
                }
            }
        }

        let entry = entries
            .entry((provider.to_string(), capability))
            .or_default();

        if let Some(fp) = fingerprint {
            if entry.fingerprint.is_none() {
                entry.fingerprint = Some(fp.to_string());
            }
        }

        // The "release one when every commercial provider is soft-down" rule
        // (provider-health 3.3). Still honour the single-probe invariant: if a
        // probe is already in flight, skip rather than open a second one.
        if is_released && entry.is_soft_down_at(now) && !entry.probing {
            entry.probing = true;
            return PermitOutcome::Probe(HealthGuard {
                registry: Arc::clone(self),
                provider: provider.to_string(),
                capability,
                is_probe: true,
                recorded: false,
            });
        }

        match entry.down_until {
            Some(until) if now < until => PermitOutcome::Skipped {
                reason: entry
                    .last_reason
                    .clone()
                    .unwrap_or_else(|| "unavailable".to_string()),
                severity: entry.last_severity.unwrap_or(FailureSeverity::Soft),
            },
            Some(_) => {
                // Window expired: exactly one caller gets probe permit
                if entry.probing {
                    PermitOutcome::Skipped {
                        reason: entry
                            .last_reason
                            .clone()
                            .unwrap_or_else(|| "unavailable".to_string()),
                        severity: entry.last_severity.unwrap_or(FailureSeverity::Soft),
                    }
                } else {
                    entry.probing = true;
                    PermitOutcome::Probe(HealthGuard {
                        registry: Arc::clone(self),
                        provider: provider.to_string(),
                        capability,
                        is_probe: true,
                        recorded: false,
                    })
                }
            }
            None => PermitOutcome::Allowed(HealthGuard {
                registry: Arc::clone(self),
                provider: provider.to_string(),
                capability,
                is_probe: false,
                recorded: false,
            }),
        }
    }

    pub fn acquire_permit_at(
        self: &Arc<Self>,
        provider: &str,
        capability: Capability,
        fingerprint: Option<&str>,
        now: Instant,
    ) -> PermitOutcome {
        self.acquire_permit_opt_release(provider, capability, fingerprint, false, now)
    }

    pub fn record_success_at(&self, provider: &str, capability: Capability, _now: Instant) {
        if !is_commercial_provider(provider) {
            return;
        }
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let entry = entries
            .entry((provider.to_string(), capability))
            .or_default();
        entry.clear_down();
    }

    pub fn record_failure_at(
        &self,
        provider: &str,
        capability: Capability,
        error: &ProviderError,
        now: Instant,
    ) {
        if !is_commercial_provider(provider) {
            return;
        }
        let action = classify_failure(capability, error);
        let FailureAction::Counted {
            severity,
            threshold,
            is_account_level,
        } = action
        else {
            // Ignored failure: release probe if active, but do not increment failure counters.
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(entry) = entries.get_mut(&(provider.to_string(), capability)) {
                entry.probing = false;
            }
            return;
        };

        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let targets = if is_account_level {
            vec![Capability::Search, Capability::Fetch]
        } else {
            vec![capability]
        };

        let code = error.code();
        for target_cap in targets {
            let entry = entries
                .entry((provider.to_string(), target_cap))
                .or_default();
            entry.probing = false;
            entry.last_reason = Some(code.clone());
            entry.last_severity = Some(severity);

            match severity {
                FailureSeverity::Soft => {
                    entry.soft_streak += 1;
                    if entry.soft_streak >= threshold {
                        entry.downs += 1;
                        let window = crate::services::service_health::backoff_for(entry.downs);
                        entry.down_until = Some(now + window);
                    }
                }
                FailureSeverity::Hard => {
                    entry.soft_streak = 0;
                    entry.downs += 1;
                    let window = hard_backoff_for(entry.downs);
                    entry.down_until = Some(now + window);
                }
            }
        }
    }

    pub fn release_probe(&self, provider: &str, capability: Capability) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = entries.get_mut(&(provider.to_string(), capability)) {
            entry.probing = false;
        }
    }

    pub fn is_soft_cooling_down_at(
        &self,
        provider: &str,
        capability: Capability,
        now: Instant,
    ) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries
            .get(&(provider.to_string(), capability))
            .is_some_and(|entry| entry.is_soft_down_at(now))
    }

    pub fn is_hard_cooling_down_at(
        &self,
        provider: &str,
        capability: Capability,
        now: Instant,
    ) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries
            .get(&(provider.to_string(), capability))
            .is_some_and(|entry| entry.is_hard_down_at(now))
    }

    pub fn all_commercial_in_soft_cooldown_at(&self, capability: Capability, now: Instant) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        COMMERCIAL_PROVIDERS.iter().all(|name| {
            entries
                .get(&(name.to_string(), capability))
                .is_some_and(|entry| entry.is_soft_down_at(now))
        })
    }

    pub fn downs(&self, provider: &str, capability: Capability) -> u32 {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries
            .get(&(provider.to_string(), capability))
            .map_or(0, |e| e.downs)
    }

    pub fn soft_streak(&self, provider: &str, capability: Capability) -> u32 {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries
            .get(&(provider.to_string(), capability))
            .map_or(0, |e| e.soft_streak)
    }

    pub fn reset(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.clear();
    }
}

impl Default for ProviderHealthRegistry {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

pub fn shared() -> &'static Arc<ProviderHealthRegistry> {
    static REGISTRY: OnceLock<Arc<ProviderHealthRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(ProviderHealthRegistry::new)
}

// ── Public module-level API using the shared process registry ──

pub fn acquire_permit(
    provider: &str,
    capability: Capability,
    fingerprint: Option<&str>,
) -> PermitOutcome {
    acquire_permit_at(provider, capability, fingerprint, Instant::now())
}

pub fn acquire_permit_at(
    provider: &str,
    capability: Capability,
    fingerprint: Option<&str>,
    now: Instant,
) -> PermitOutcome {
    shared().acquire_permit_at(provider, capability, fingerprint, now)
}

pub fn acquire_permit_opt_release(
    provider: &str,
    capability: Capability,
    fingerprint: Option<&str>,
    is_released: bool,
) -> PermitOutcome {
    acquire_permit_opt_release_at(
        provider,
        capability,
        fingerprint,
        is_released,
        Instant::now(),
    )
}

pub fn acquire_permit_opt_release_at(
    provider: &str,
    capability: Capability,
    fingerprint: Option<&str>,
    is_released: bool,
    now: Instant,
) -> PermitOutcome {
    shared().acquire_permit_opt_release(provider, capability, fingerprint, is_released, now)
}

pub fn record_success(provider: &str, capability: Capability) {
    record_success_at(provider, capability, Instant::now());
}

pub fn record_success_at(provider: &str, capability: Capability, now: Instant) {
    shared().record_success_at(provider, capability, now);
}

pub fn record_failure(provider: &str, capability: Capability, error: &ProviderError) {
    record_failure_at(provider, capability, error, Instant::now());
}

pub fn record_failure_at(
    provider: &str,
    capability: Capability,
    error: &ProviderError,
    now: Instant,
) {
    shared().record_failure_at(provider, capability, error, now);
}

pub fn release_probe(provider: &str, capability: Capability) {
    shared().release_probe(provider, capability);
}

pub fn is_soft_cooling_down_at(provider: &str, capability: Capability, now: Instant) -> bool {
    shared().is_soft_cooling_down_at(provider, capability, now)
}

pub fn is_hard_cooling_down_at(provider: &str, capability: Capability, now: Instant) -> bool {
    shared().is_hard_cooling_down_at(provider, capability, now)
}

pub fn all_commercial_in_soft_cooldown(capability: Capability) -> bool {
    all_commercial_in_soft_cooldown_at(capability, Instant::now())
}

pub fn all_commercial_in_soft_cooldown_at(capability: Capability, now: Instant) -> bool {
    shared().all_commercial_in_soft_cooldown_at(capability, now)
}

pub fn reset() {
    shared().reset();
}

/// Test-only serialisation for the process-wide registry plus a clean reset.
///
/// Search and fetch tests in different modules share this registry; holding the
/// lock keeps one test's cooldowns from skipping another test's provider call.
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    shared().reset();
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_maps_errors_correctly() {
        assert_eq!(
            classify_failure(Capability::Search, &ProviderError::Timeout),
            FailureAction::Counted {
                severity: FailureSeverity::Soft,
                threshold: 2,
                is_account_level: false,
            }
        );
        assert_eq!(
            classify_failure(Capability::Search, &ProviderError::RateLimited),
            FailureAction::Counted {
                severity: FailureSeverity::Soft,
                threshold: 1,
                is_account_level: false,
            }
        );
        assert_eq!(
            classify_failure(Capability::Search, &ProviderError::Auth),
            FailureAction::Counted {
                severity: FailureSeverity::Hard,
                threshold: 1,
                is_account_level: true,
            }
        );
        assert_eq!(
            classify_failure(Capability::Search, &ProviderError::QuotaExhausted),
            FailureAction::Counted {
                severity: FailureSeverity::Hard,
                threshold: 1,
                is_account_level: true,
            }
        );
        // Search 403 is Auth (Hard)
        assert_eq!(
            classify_failure(Capability::Search, &ProviderError::Http(403)),
            FailureAction::Counted {
                severity: FailureSeverity::Hard,
                threshold: 1,
                is_account_level: true,
            }
        );
        // Fetch 403 is Ignored
        assert_eq!(
            classify_failure(Capability::Fetch, &ProviderError::Http(403)),
            FailureAction::Ignored
        );
        // Fetch 404 is Ignored
        assert_eq!(
            classify_failure(Capability::Fetch, &ProviderError::Http(404)),
            FailureAction::Ignored
        );
        // Fetch 401 is Hard
        assert_eq!(
            classify_failure(Capability::Fetch, &ProviderError::Http(401)),
            FailureAction::Counted {
                severity: FailureSeverity::Hard,
                threshold: 1,
                is_account_level: true,
            }
        );
        // 400 Bad Request is Ignored
        assert_eq!(
            classify_failure(Capability::Search, &ProviderError::Http(400)),
            FailureAction::Ignored
        );
    }

    #[test]
    fn hard_backoff_doubles_and_caps_at_one_hour() {
        assert_eq!(hard_backoff_for(1), Duration::from_secs(600));
        assert_eq!(hard_backoff_for(2), Duration::from_secs(1200));
        assert_eq!(hard_backoff_for(3), Duration::from_secs(2400));
        assert_eq!(hard_backoff_for(4), Duration::from_secs(3600));
        assert_eq!(hard_backoff_for(5), Duration::from_secs(3600));
    }

    #[test]
    fn acceptance_4_window_expiration_probe_concurrency_and_backoff_doubling() {
        let registry = ProviderHealthRegistry::new();
        let t0 = Instant::now();

        // 429 enters Down immediately with 30s backoff (downs=1)
        registry.record_failure_at("brave", Capability::Search, &ProviderError::RateLimited, t0);
        assert_eq!(registry.downs("brave", Capability::Search), 1);
        assert!(registry.is_soft_cooling_down_at(
            "brave",
            Capability::Search,
            t0 + Duration::from_secs(10)
        ));

        // Within window: skipped
        match registry.acquire_permit_at(
            "brave",
            Capability::Search,
            None,
            t0 + Duration::from_secs(10),
        ) {
            PermitOutcome::Skipped { reason, severity } => {
                assert_eq!(reason, "rate_limited");
                assert_eq!(severity, FailureSeverity::Soft);
            }
            other => panic!("expected skipped, got {other:?}"),
        }

        // Window expired at t0 + 31s: first caller gets Probe
        let t_probe = t0 + Duration::from_secs(31);
        let guard1 = match registry.acquire_permit_at("brave", Capability::Search, None, t_probe) {
            PermitOutcome::Probe(guard) => guard,
            other => panic!("expected probe, got {other:?}"),
        };

        // Second concurrent caller is skipped because probing is active
        match registry.acquire_permit_at("brave", Capability::Search, None, t_probe) {
            PermitOutcome::Skipped { .. } => {}
            other => panic!("expected second permit to be skipped, got {other:?}"),
        }

        // Probe fails -> downs increments to 2, backoff doubles to 60s
        guard1.record_failure_at(&ProviderError::RateLimited, t_probe);
        assert_eq!(registry.downs("brave", Capability::Search), 2);
        // At t_probe + 50s, still in backoff (60s total)
        assert!(registry.is_soft_cooling_down_at(
            "brave",
            Capability::Search,
            t_probe + Duration::from_secs(50)
        ));

        // Expired after 61s from t_probe: next caller probes and succeeds -> resets to 0
        let t_probe2 = t_probe + Duration::from_secs(61);
        let guard2 = match registry.acquire_permit_at("brave", Capability::Search, None, t_probe2) {
            PermitOutcome::Probe(guard) => guard,
            other => panic!("expected probe, got {other:?}"),
        };
        guard2.record_success_at(t_probe2);
        assert_eq!(registry.downs("brave", Capability::Search), 0);
        assert!(!registry.is_soft_cooling_down_at("brave", Capability::Search, t_probe2));
    }

    #[test]
    fn acceptance_5_guard_drop_releases_probe_without_recording() {
        let registry = ProviderHealthRegistry::new();
        let t0 = Instant::now();

        registry.record_failure_at("exa", Capability::Search, &ProviderError::RateLimited, t0);
        let t_probe = t0 + Duration::from_secs(31);

        {
            let guard = match registry.acquire_permit_at("exa", Capability::Search, None, t_probe) {
                PermitOutcome::Probe(guard) => guard,
                other => panic!("expected probe, got {other:?}"),
            };
            assert!(guard.is_probe());
            // Drop without recording (simulates cancellation / timeout / panic)
        }

        // Probing should be released immediately so next call can probe
        let guard2 = match registry.acquire_permit_at("exa", Capability::Search, None, t_probe) {
            PermitOutcome::Probe(guard) => guard,
            other => panic!("expected probe after drop, got {other:?}"),
        };
        // Downs should not have changed
        assert_eq!(registry.downs("exa", Capability::Search), 1);
        guard2.record_success_at(t_probe);
        assert_eq!(registry.downs("exa", Capability::Search), 0);
    }

    #[test]
    fn acceptance_7_fingerprint_clears_down_immediately_on_key_change() {
        let registry = ProviderHealthRegistry::new();
        let t0 = Instant::now();
        let fp1 = compute_fingerprint("https://api.exa.ai", Some("key1"));
        let fp2 = compute_fingerprint("https://api.exa.ai", Some("key2"));
        assert_ne!(fp1, fp2);
        assert_eq!(fp1.len(), 16);

        // First call with fp1
        match registry.acquire_permit_at("exa", Capability::Search, Some(&fp1), t0) {
            PermitOutcome::Allowed(guard) => {
                guard.record_failure_at(&ProviderError::QuotaExhausted, t0)
            }
            other => panic!("expected allowed, got {other:?}"),
        }
        assert!(registry.is_hard_cooling_down_at(
            "exa",
            Capability::Search,
            t0 + Duration::from_secs(10)
        ));
        assert!(registry.is_hard_cooling_down_at(
            "exa",
            Capability::Fetch,
            t0 + Duration::from_secs(10)
        ));

        // With new fingerprint fp2, Down is immediately cleared across all capabilities!
        match registry.acquire_permit_at(
            "exa",
            Capability::Search,
            Some(&fp2),
            t0 + Duration::from_secs(10),
        ) {
            PermitOutcome::Allowed(_) => {}
            other => panic!("expected allowed on key change, got {other:?}"),
        }
        assert!(!registry.is_hard_cooling_down_at(
            "exa",
            Capability::Search,
            t0 + Duration::from_secs(10)
        ));
        assert!(!registry.is_hard_cooling_down_at(
            "exa",
            Capability::Fetch,
            t0 + Duration::from_secs(10)
        ));
    }

    #[test]
    fn acceptance_9_state_machine_200_zero_results_and_400_ignored() {
        let registry = ProviderHealthRegistry::new();
        let t0 = Instant::now();

        // 1st soft failure -> soft_streak = 1, not down
        registry.record_failure_at("deepseek", Capability::Search, &ProviderError::Timeout, t0);
        assert_eq!(registry.soft_streak("deepseek", Capability::Search), 1);
        assert_eq!(registry.downs("deepseek", Capability::Search), 0);

        // 400 Bad Request does not count -> soft_streak stays 1
        registry.record_failure_at(
            "deepseek",
            Capability::Search,
            &ProviderError::Http(400),
            t0,
        );
        assert_eq!(registry.soft_streak("deepseek", Capability::Search), 1);

        // 200 with zero results counts as success -> soft_streak reset to 0
        registry.record_success_at("deepseek", Capability::Search, t0);
        assert_eq!(registry.soft_streak("deepseek", Capability::Search), 0);
    }

    #[test]
    fn acceptance_11_capabilities_isolated_except_auth_and_quota() {
        let registry = ProviderHealthRegistry::new();
        let t0 = Instant::now();

        // 1. Soft failure on Search does not affect Fetch
        registry.record_failure_at(
            "firecrawl",
            Capability::Search,
            &ProviderError::RateLimited,
            t0,
        );
        assert!(registry.is_soft_cooling_down_at(
            "firecrawl",
            Capability::Search,
            t0 + Duration::from_secs(5)
        ));
        assert!(!registry.is_soft_cooling_down_at(
            "firecrawl",
            Capability::Fetch,
            t0 + Duration::from_secs(5)
        ));

        // Success on Search does not clear Fetch
        registry.record_failure_at(
            "firecrawl",
            Capability::Fetch,
            &ProviderError::RateLimited,
            t0,
        );
        registry.record_success_at("firecrawl", Capability::Search, t0);
        assert!(!registry.is_soft_cooling_down_at(
            "firecrawl",
            Capability::Search,
            t0 + Duration::from_secs(5)
        ));
        assert!(registry.is_soft_cooling_down_at(
            "firecrawl",
            Capability::Fetch,
            t0 + Duration::from_secs(5)
        ));

        // 2. Auth on Search writes to BOTH Search and Fetch
        registry.record_failure_at("tavily", Capability::Search, &ProviderError::Auth, t0);
        assert!(registry.is_hard_cooling_down_at(
            "tavily",
            Capability::Search,
            t0 + Duration::from_secs(5)
        ));
        assert!(registry.is_hard_cooling_down_at(
            "tavily",
            Capability::Fetch,
            t0 + Duration::from_secs(5)
        ));

        // 3. QuotaExhausted on Fetch writes to BOTH Search and Fetch
        let registry2 = ProviderHealthRegistry::new();
        registry2.record_failure_at(
            "anysearch",
            Capability::Fetch,
            &ProviderError::QuotaExhausted,
            t0,
        );
        assert!(registry2.is_hard_cooling_down_at(
            "anysearch",
            Capability::Search,
            t0 + Duration::from_secs(5)
        ));
        assert!(registry2.is_hard_cooling_down_at(
            "anysearch",
            Capability::Fetch,
            t0 + Duration::from_secs(5)
        ));
    }
}
