//! Unified retry controller, fallback chain and switch receipts.
//!
//! @nyan: docs/nyan/provider-redesign.md §7.2 and decisions 11, 19, 23.
//!
//! One place decides what a failed attempt means, for every protocol:
//!
//! - an adapter's [`RatePolicy`] says whether a failure is worth repeating
//!   against the same endpoint, only worth handing to a fallback, or neither;
//! - [`judge`] applies the delay ceiling and the rules no policy may override:
//!   a stream that already produced output is never replayed, and an abort is
//!   never retried;
//! - [`RetryLimits`] is the budget: default and global cap 2.
//!   The loop that spends it is `services/api/with_retry.rs`;
//! - [`fallback_chain`] turns the explicit `selection.mainFallbacks` list into
//!   profiles. An empty list is an empty chain: nothing switches provider on
//!   its own;
//! - [`SwitchReceipt`] records a switch without any header, body or credential.

use std::time::Duration;

use futures::StreamExt;
use serde::Serialize;

use super::adapter::{Cancel, EventStream, ResolvedTarget};
use super::ir::{Protocol, ProviderError, ProviderErrorKind, StreamEvent, StreamEventKind};
use crate::utils::model::profile::{ResolvedModelProfile, resolve_profile};
use crate::utils::settings::nyan::{NyanSettings, RetrySettings};

/// Retries per profile when nothing else says so, and the global cap no
/// profile or environment value can exceed (decision 19). @nyan: decision 19
/// let managed settings move the cap (up to 5); with no managed policy layer
/// the cap is this default.
pub const DEFAULT_MAX_RETRIES: u32 = 2;
/// The longest server-hinted wait any profile may ask for.
pub const DEFAULT_MAX_RETRY_DELAY: Duration = Duration::from_secs(32);

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryLimits {
    pub max_retries: u32,
    /// The longest wait a server hint may ask for before the attempt is given
    /// up instead of slept on.
    pub max_delay: Duration,
}

impl RetryLimits {
    /// `profile` is what the profile asks for, `env_default` the process-wide
    /// `CLAUDE_CODE_MAX_RETRIES`. Both can lower the budget and the delay
    /// ceiling but never raise them past the global defaults.
    pub fn resolve(profile: Option<&RetrySettings>, env_default: Option<u32>) -> Self {
        let requested = profile
            .and_then(|retry| retry.max_retries)
            .or(env_default)
            .unwrap_or(DEFAULT_MAX_RETRIES);
        let max_delay = profile
            .and_then(|retry| retry.max_retry_delay_ms)
            .map(Duration::from_millis)
            .map_or(DEFAULT_MAX_RETRY_DELAY, |delay| {
                delay.min(DEFAULT_MAX_RETRY_DELAY)
            });
        Self {
            max_retries: requested.min(DEFAULT_MAX_RETRIES),
            max_delay,
        }
    }

    /// For a request that names no profile: the default budget under the cap.
    pub fn unprofiled() -> Self {
        Self::resolve(None, env_max_retries())
    }

    pub fn for_profile(profile: &str) -> Self {
        Self::resolve(profile_retry_settings(profile).as_ref(), env_max_retries())
    }

    pub fn for_profile_with_override(profile: &str, retries_override: Option<u32>) -> Self {
        let mut limits = Self::for_profile(profile);
        if let Some(r) = retries_override {
            limits.max_retries = r.min(DEFAULT_MAX_RETRIES);
        }
        limits
    }
}

/// Budget for call sites that name no profile (side queries, the non-streaming
/// request): the same default and the same cap.
pub fn default_max_retries() -> u32 {
    RetryLimits::unprofiled().max_retries
}

fn env_max_retries() -> Option<u32> {
    crate::utils::process_env::var("NYAN_MAX_RETRIES").and_then(|raw| raw.trim().parse().ok())
}

fn profile_retry_settings(name: &str) -> Option<RetrySettings> {
    let settings = crate::utils::settings::get_initial_settings();
    crate::utils::model::profile::model_profile(settings.nyan.as_deref()?, name)?
        .retry
        .clone()
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Repeat against the same endpoint after `wait` (the server's hint), or
    /// after backoff when there is none.
    Retry { wait: Option<Duration> },
    /// Repeating cannot help (a billing 429), but a configured fallback may.
    SwitchOnly,
    /// Neither a retry nor a fallback can help.
    Fatal,
}

/// What one protocol's failures mean. Header and body details belong to the
/// adapter that understands them; nothing here treats one vendor's header as a
/// general protocol.
pub trait RatePolicy: Send + Sync {
    fn classify(&self, error: &ProviderError) -> Verdict;
}

/// The kind table every policy starts from: only what is plainly transient is
/// worth repeating.
pub fn base_verdict(error: &ProviderError) -> Verdict {
    match &error.kind {
        ProviderErrorKind::RateLimited
        | ProviderErrorKind::Overloaded
        | ProviderErrorKind::Transient { retryable: true } => Verdict::Retry {
            wait: error.retry_after,
        },
        ProviderErrorKind::Transport => Verdict::Retry { wait: None },
        ProviderErrorKind::Transient { retryable: false }
        | ProviderErrorKind::ContextOverflow
        | ProviderErrorKind::InvalidRequest
        | ProviderErrorKind::Unauthorized
        | ProviderErrorKind::Forbidden
        | ProviderErrorKind::NotFound
        | ProviderErrorKind::PartialStream
        | ProviderErrorKind::Aborted => Verdict::Fatal,
    }
}

/// Plain HTTP semantics, for a protocol whose adapter has no policy of its own.
pub struct StandardPolicy;

impl RatePolicy for StandardPolicy {
    fn classify(&self, error: &ProviderError) -> Verdict {
        base_verdict(error)
    }
}

pub fn policy_for(protocol: Protocol) -> &'static dyn RatePolicy {
    match protocol {
        Protocol::AnthropicMessages => &super::anthropic::policy::AnthropicPolicy,
        Protocol::OpenaiChat | Protocol::OpenaiResponses => &super::openai::policy::OpenAiPolicy,
        Protocol::Gemini => &super::gemini::policy::GeminiPolicy,
    }
}

// ---------------------------------------------------------------------------
// Judgement
// ---------------------------------------------------------------------------

/// Why a failure is not retried against the same endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GiveUp {
    Fatal,
    SwitchOnly,
    /// The server asked for longer than `maxRetryDelay` allows.
    WaitTooLong,
}

/// An attempt's failure after the policy has looked at it. `stop` is `None`
/// when the loop may retry; the loop's own budget then decides how often.
#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    pub error: ProviderError,
    pub stop: Option<GiveUp>,
}

impl Judged {
    /// The verdict the retry loop acts on. A wait longer than the ceiling is
    /// not worth sleeping on, but a fallback may still help, so it reads as
    /// switch-only.
    pub fn verdict(&self) -> Verdict {
        match self.stop {
            None => Verdict::Retry {
                wait: self.error.retry_after,
            },
            Some(GiveUp::Fatal) => Verdict::Fatal,
            Some(GiveUp::SwitchOnly | GiveUp::WaitTooLong) => Verdict::SwitchOnly,
        }
    }
}

pub fn judge(policy: &dyn RatePolicy, limits: &RetryLimits, mut error: ProviderError) -> Judged {
    // No policy may override these two (§7.2): output already produced means a
    // replay could repeat tool calls, and an abort is the user's choice.
    if matches!(
        error.kind,
        ProviderErrorKind::PartialStream | ProviderErrorKind::Aborted
    ) {
        return Judged {
            error,
            stop: Some(GiveUp::Fatal),
        };
    }
    let stop = match policy.classify(&error) {
        Verdict::Fatal => Some(GiveUp::Fatal),
        Verdict::SwitchOnly => Some(GiveUp::SwitchOnly),
        Verdict::Retry { wait } => {
            error.retry_after = wait;
            wait.filter(|wait| *wait > limits.max_delay)
                .map(|_| GiveUp::WaitTooLong)
        }
    };
    Judged { error, stop }
}

// ---------------------------------------------------------------------------
// First-output commit
// ---------------------------------------------------------------------------

/// Whether an event is output a user could see or a tool call could start from.
fn is_output(kind: &StreamEventKind) -> bool {
    matches!(
        kind,
        StreamEventKind::BlockStart { .. }
            | StreamEventKind::BlockDelta { .. }
            | StreamEventKind::BlockStop { .. }
    )
}

/// Hold a freshly opened stream back until it produces its first output or
/// ends. A failure before that point, such as a rate-limit `error` event inside
/// a 200 response, is still an open failure: nothing was shown, so it may be
/// retried. After the commit point the caller owns the stream and a failure is
/// the adapter's `PartialStream`.
///
/// `idle` bounds the silence between events, standing in for the stream
/// watchdog that only starts once the caller owns the stream.
pub async fn commit_first_output(
    mut events: EventStream,
    cancel: &Cancel,
    idle: Option<Duration>,
) -> Result<EventStream, ProviderError> {
    let mut held: Vec<StreamEvent> = Vec::new();
    let mut abort = cancel.clone();
    loop {
        let silence = async {
            match idle {
                Some(idle) => tokio::time::sleep(idle).await,
                None => futures::future::pending().await,
            }
        };
        let next = tokio::select! {
            biased;
            _ = abort.aborted() => {
                return Err(ProviderError::new(ProviderErrorKind::Aborted, "Request was aborted."));
            }
            _ = silence => {
                return Err(ProviderError::new(
                    ProviderErrorKind::Transport,
                    "Stream idle timeout - no chunks received",
                ));
            }
            next = events.next() => next,
        };
        match next {
            Some(Err(error)) => return Err(error),
            Some(Ok(event)) => {
                let output = is_output(&event.kind);
                held.push(event);
                if output {
                    break;
                }
            }
            None => break,
        }
    }
    Ok(Box::pin(
        futures::stream::iter(held.into_iter().map(Ok)).chain(events),
    ))
}

// ---------------------------------------------------------------------------
// Fallback chain and switch receipt
// ---------------------------------------------------------------------------

/// The profiles `selection.mainFallbacks` names, in order, each resolved with
/// its own protocol, credential reference and capabilities. The primary and
/// repeats are dropped. An entry that does not resolve comes back in the second
/// list with the reason; it is never replaced by a guess.
pub fn fallback_chain(
    nyan: &NyanSettings,
    primary: &ResolvedModelProfile,
) -> (Vec<ResolvedModelProfile>, Vec<(String, String)>) {
    let mut chain: Vec<ResolvedModelProfile> = Vec::new();
    let mut skipped = Vec::new();
    let names = nyan
        .selection
        .as_ref()
        .and_then(|selection| selection.main_fallbacks.as_deref())
        .unwrap_or_default();
    for name in names {
        if name == &primary.profile || chain.iter().any(|profile| &profile.profile == name) {
            continue;
        }
        match resolve_profile(nyan, name) {
            Ok(profile) => chain.push(profile),
            Err(error) => skipped.push((name.clone(), error.to_string())),
        }
    }
    (chain, skipped)
}

/// Why and where a request moved to another profile. Carries no header, body,
/// message text or credential, so it is safe to log and to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SwitchReceipt {
    pub from_profile: String,
    pub from_provider: String,
    pub from_protocol: Protocol,
    pub to_profile: String,
    pub to_provider: String,
    pub to_protocol: Protocol,
    pub error_kind: ProviderErrorKind,
    pub status: Option<u16>,
    pub request_id: Option<String>,
}

impl SwitchReceipt {
    pub fn new(
        from: &ResolvedModelProfile,
        to: &ResolvedModelProfile,
        error_kind: ProviderErrorKind,
        status: Option<u16>,
        request_id: Option<String>,
    ) -> Self {
        Self {
            from_profile: from.profile.clone(),
            from_provider: from.provider.clone(),
            from_protocol: ResolvedTarget::from_profile(from).protocol,
            to_profile: to.profile.clone(),
            to_provider: to.provider.clone(),
            to_protocol: ResolvedTarget::from_profile(to).protocol,
            error_kind,
            status,
            request_id,
        }
    }

    /// One line for the transcript.
    pub fn notice(&self) -> String {
        let status = self
            .status
            .map(|status| format!(" (HTTP {status})"))
            .unwrap_or_default();
        format!(
            "Switched from profile {:?} to fallback profile {:?} after a {:?} failure{status}.",
            self.from_profile, self.to_profile, self.error_kind
        )
    }
}

/// Turns an adapter's [`ProviderError`] into [`crate::services::api::with_retry::RetryableError`]
/// judged by the protocol's rate policy.
pub fn to_retryable(
    error: ProviderError,
    protocol: Protocol,
    limits: &RetryLimits,
) -> crate::services::api::with_retry::RetryableError {
    use crate::services::api::with_retry::{ApiError, ConnectionError, RetryableError};

    match error.kind {
        ProviderErrorKind::Aborted => RetryableError::Aborted,
        ProviderErrorKind::Transport => RetryableError::Connection(ConnectionError {
            message: error.message,
            code: None,
        }),
        ProviderErrorKind::InvalidRequest if error.status.is_none() => {
            RetryableError::Other(anyhow::anyhow!(error.message))
        }
        _ => {
            let judged = judge(policy_for(protocol), limits, error);
            let verdict = judged.verdict();
            let error = judged.error;
            RetryableError::Judged {
                api: ApiError {
                    status: error.status,
                    message: error.message,
                    headers: error.headers.into_iter().collect(),
                    body: error.body,
                },
                verdict,
                kind: error.kind,
                request_id: error.request_id,
                max_delay: limits.max_delay,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::services::provider::ir::{BlockStart, Delta, Usage};

    fn settings(max_retries: Option<u32>, delay_ms: Option<u64>) -> RetrySettings {
        RetrySettings {
            max_retries,
            max_retry_delay_ms: delay_ms,
        }
    }

    fn limits(max_retries: u32) -> RetryLimits {
        RetryLimits {
            max_retries,
            max_delay: Duration::from_secs(32),
        }
    }

    fn limited(wait: Option<Duration>) -> ProviderError {
        let mut error = ProviderError::new(ProviderErrorKind::RateLimited, "slow");
        error.status = Some(429);
        error.retry_after = wait;
        error
    }

    #[test]
    fn the_default_budget_is_two() {
        assert_eq!(RetryLimits::resolve(None, None).max_retries, 2);
    }

    #[test]
    fn a_profile_can_lower_the_budget_but_not_exceed_the_cap() {
        for (asked, expected) in [(0, 0), (1, 1), (2, 2), (5, 2), (100, 2)] {
            let profile = settings(Some(asked), None);
            assert_eq!(
                RetryLimits::resolve(Some(&profile), None).max_retries,
                expected,
                "profile asked for {asked}"
            );
        }
    }

    #[test]
    fn the_environment_is_held_to_the_cap_and_the_profile_overrides_it() {
        assert_eq!(RetryLimits::resolve(None, Some(10)).max_retries, 2);
        assert_eq!(RetryLimits::resolve(None, Some(1)).max_retries, 1);
        let profile = settings(Some(0), None);
        assert_eq!(
            RetryLimits::resolve(Some(&profile), Some(10)).max_retries,
            0
        );
    }

    #[test]
    fn a_profile_delay_ceiling_cannot_exceed_the_default_one() {
        let profile = settings(None, Some(60_000));
        assert_eq!(
            RetryLimits::resolve(Some(&profile), None).max_delay,
            DEFAULT_MAX_RETRY_DELAY
        );
        let lower = settings(None, Some(1_000));
        assert_eq!(
            RetryLimits::resolve(Some(&lower), None).max_delay,
            Duration::from_secs(1)
        );
        assert_eq!(
            RetryLimits::resolve(None, None).max_delay,
            DEFAULT_MAX_RETRY_DELAY
        );
    }

    #[test]
    fn retry_after_is_the_wait_and_an_over_long_one_gives_up() {
        let policy = StandardPolicy;
        let within = judge(&policy, &limits(2), limited(Some(Duration::from_secs(7))));
        assert_eq!(within.stop, None);
        assert_eq!(
            within.verdict(),
            Verdict::Retry {
                wait: Some(Duration::from_secs(7))
            }
        );
        let over = judge(&policy, &limits(2), limited(Some(Duration::from_secs(120))));
        assert_eq!(over.stop, Some(GiveUp::WaitTooLong));
        // Waiting is pointless but another profile may serve it.
        assert_eq!(over.verdict(), Verdict::SwitchOnly);
    }

    #[test]
    fn overloaded_and_service_unavailable_are_retried_without_a_hint() {
        let policy = StandardPolicy;
        for (kind, status) in [
            (ProviderErrorKind::Overloaded, 529),
            (ProviderErrorKind::Transient { retryable: true }, 503),
        ] {
            let mut error = ProviderError::new(kind, "busy");
            error.status = Some(status);
            let judged = judge(&policy, &limits(2), error);
            assert_eq!(judged.stop, None, "status {status}");
            assert_eq!(judged.verdict(), Verdict::Retry { wait: None });
        }
    }

    #[test]
    fn a_partial_stream_and_an_abort_are_never_retried_whatever_the_policy_says() {
        struct Eager;
        impl RatePolicy for Eager {
            fn classify(&self, _: &ProviderError) -> Verdict {
                Verdict::Retry { wait: None }
            }
        }
        for kind in [ProviderErrorKind::PartialStream, ProviderErrorKind::Aborted] {
            let judged = judge(&Eager, &limits(5), ProviderError::new(kind, "x"));
            assert_eq!(judged.stop, Some(GiveUp::Fatal));
            assert_eq!(judged.verdict(), Verdict::Fatal);
        }
    }

    #[test]
    fn client_errors_are_fatal_and_cannot_fail_over() {
        for kind in [
            ProviderErrorKind::InvalidRequest,
            ProviderErrorKind::Unauthorized,
            ProviderErrorKind::Forbidden,
            ProviderErrorKind::NotFound,
            ProviderErrorKind::ContextOverflow,
        ] {
            let judged = judge(&StandardPolicy, &limits(2), ProviderError::new(kind, "x"));
            assert_eq!(judged.verdict(), Verdict::Fatal);
        }
    }

    #[test]
    fn every_protocol_has_a_policy_and_billing_is_openai_only() {
        let mut quota = limited(None);
        quota.body = Some(json!({"error": {"code": "insufficient_quota"}}));
        assert_eq!(
            policy_for(Protocol::OpenaiChat).classify(&quota),
            Verdict::SwitchOnly
        );
        assert_eq!(
            policy_for(Protocol::OpenaiResponses).classify(&quota),
            Verdict::SwitchOnly
        );
        // The same body on a protocol that does not define the code is a burst.
        assert_eq!(
            policy_for(Protocol::AnthropicMessages).classify(&quota),
            Verdict::Retry { wait: None }
        );
        assert_eq!(
            policy_for(Protocol::Gemini).classify(&quota),
            Verdict::Retry { wait: None }
        );
        // Gemini recognizes its own quota and billing exhaustion signals.
        let mut gemini_quota = limited(None);
        gemini_quota.message = "Resource has been exhausted: quota exceeded".into();
        assert_eq!(
            policy_for(Protocol::Gemini).classify(&gemini_quota),
            Verdict::SwitchOnly
        );
    }

    fn event(kind: StreamEventKind) -> Result<StreamEvent, ProviderError> {
        Ok(StreamEvent::new(kind))
    }

    fn start() -> StreamEventKind {
        StreamEventKind::MessageStart {
            request_id: None,
            message_id: None,
            model: None,
            usage: Usage::default(),
        }
    }

    fn boxed(items: Vec<Result<StreamEvent, ProviderError>>) -> EventStream {
        Box::pin(futures::stream::iter(items))
    }

    #[tokio::test]
    async fn an_error_before_any_output_is_an_open_failure() {
        let cancel = Cancel::new();
        let failure = ProviderError::new(ProviderErrorKind::RateLimited, "in-stream 429");
        let result = commit_first_output(
            boxed(vec![event(start()), Err(failure.clone())]),
            &cancel,
            None,
        )
        .await;
        assert_eq!(result.err(), Some(failure));
    }

    #[tokio::test]
    async fn an_error_after_output_stays_in_the_stream_for_the_caller() {
        let cancel = Cancel::new();
        let partial = ProviderError::new(ProviderErrorKind::PartialStream, "cut");
        let text = StreamEventKind::BlockStart {
            index: 0,
            block: BlockStart::Text,
        };
        let delta = StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Text { text: "hi".into() },
        };
        let committed = commit_first_output(
            boxed(vec![
                event(start()),
                event(text.clone()),
                event(delta.clone()),
                Err(partial.clone()),
            ]),
            &cancel,
            None,
        )
        .await
        .expect("output was produced, so the open succeeded");
        let rest: Vec<_> = committed.collect().await;
        assert_eq!(rest.len(), 4);
        assert_eq!(rest[1].as_ref().unwrap().kind, text);
        assert_eq!(rest[2].as_ref().unwrap().kind, delta);
        assert_eq!(rest[3].as_ref().unwrap_err(), &partial);
    }

    #[tokio::test]
    async fn a_silent_stream_times_out_as_a_transport_failure() {
        let cancel = Cancel::new();
        let silent: EventStream = Box::pin(futures::stream::pending());
        let result = commit_first_output(silent, &cancel, Some(Duration::from_millis(20))).await;
        assert_eq!(
            result.err().map(|e| e.kind),
            Some(ProviderErrorKind::Transport)
        );
    }

    #[tokio::test]
    async fn an_abort_while_waiting_for_first_output_stops_at_once() {
        let (handle, cancel) = Cancel::pair();
        let silent: EventStream = Box::pin(futures::stream::pending());
        let wait = tokio::spawn(async move { commit_first_output(silent, &cancel, None).await });
        handle.abort();
        let result = wait.await.unwrap();
        assert_eq!(
            result.err().map(|e| e.kind),
            Some(ProviderErrorKind::Aborted)
        );
    }

    fn sample_profile(
        name: &str,
        provider: &str,
        protocol: crate::utils::settings::nyan::ProviderProtocol,
    ) -> ResolvedModelProfile {
        ResolvedModelProfile {
            profile: name.into(),
            provider: provider.into(),
            provider_kind: crate::utils::settings::nyan::ProviderKind::Openai,
            protocol,
            model: "model-x".into(),
            context_window: 100_000,
            max_output_tokens: 4_000,
            capabilities: Default::default(),
            base_url: None,
            credential: None,
            compat: None,
            cache_retention: Default::default(),
            effort: None,
            extra: None,
        }
    }

    #[test]
    fn fallback_chain_is_empty_when_no_fallbacks_are_configured() {
        let nyan = NyanSettings::default();
        let primary = sample_profile(
            "main",
            "openai",
            crate::utils::settings::nyan::ProviderProtocol::OpenaiChat,
        );
        let (chain, skipped) = fallback_chain(&nyan, &primary);
        assert!(chain.is_empty());
        assert!(skipped.is_empty());
    }

    #[test]
    fn fallback_chain_drops_self_and_duplicates_and_records_unresolvable_entries() {
        use crate::utils::settings::nyan::{
            ModelCapabilities, ModelProfileSettings, ProviderSettings, SelectionSettings,
        };
        use std::collections::BTreeMap;

        let mut providers = BTreeMap::new();
        providers.insert(
            "p1".into(),
            ProviderSettings {
                kind: Some(crate::utils::settings::nyan::ProviderKind::Openai),
                protocols: Some(vec![
                    crate::utils::settings::nyan::ProviderProtocol::OpenaiChat,
                ]),
                ..Default::default()
            },
        );
        let mut models = BTreeMap::new();
        models.insert(
            "fb1".into(),
            ModelProfileSettings {
                provider: Some("p1".into()),
                model: Some("gpt-4o".into()),
                capabilities: Some(ModelCapabilities {
                    context_window: Some(128_000),
                    max_output_tokens: Some(4_096),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let nyan = NyanSettings {
            providers: Some(providers),
            models: Some(models),
            selection: Some(SelectionSettings {
                main_fallbacks: Some(vec![
                    "main".into(),        // drops self
                    "fb1".into(),         // valid
                    "fb1".into(),         // drops duplicate
                    "nonexistent".into(), // skipped with reason
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let primary = sample_profile(
            "main",
            "openai",
            crate::utils::settings::nyan::ProviderProtocol::OpenaiChat,
        );
        let (chain, skipped) = fallback_chain(&nyan, &primary);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].profile, "fb1");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, "nonexistent");
    }

    #[test]
    fn switch_receipt_carries_only_safe_metadata_and_formats_a_notice() {
        let from = sample_profile(
            "main",
            "anthropic",
            crate::utils::settings::nyan::ProviderProtocol::AnthropicMessages,
        );
        let to = sample_profile(
            "fb1",
            "openai",
            crate::utils::settings::nyan::ProviderProtocol::OpenaiChat,
        );
        let receipt = SwitchReceipt::new(
            &from,
            &to,
            ProviderErrorKind::RateLimited,
            Some(429),
            Some("req_123".into()),
        );
        assert_eq!(receipt.from_profile, "main");
        assert_eq!(receipt.to_profile, "fb1");
        assert_eq!(receipt.from_protocol, Protocol::AnthropicMessages);
        assert_eq!(receipt.to_protocol, Protocol::OpenaiChat);
        assert_eq!(receipt.status, Some(429));
        assert_eq!(receipt.request_id.as_deref(), Some("req_123"));

        let notice = receipt.notice();
        assert!(notice.contains("Switched from profile \"main\" to fallback profile \"fb1\""));
        assert!(notice.contains("RateLimited failure (HTTP 429)"));
    }
}
