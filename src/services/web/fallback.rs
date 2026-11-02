//! Ordered FetchProvider fallback after nyan's own fetcher (external-search 4.6).
//!
//! @nyan: nyan fetches first. Only a named, retryable failure (403 / 429 / 451 /
//! timeout) or a too-short body moves on to providers, in `fetchFallback` order,
//! first success wins. SSRF and configuration denials (`denied_*`,
//! `proxy_unconfigured`) never reach a provider: the URL itself must not leave
//! the machine.

use std::time::{Duration, Instant};

use chrono::Utc;

use super::budget;
use super::decision::WebDecisions;
use super::fetcher::{FetchConfig, FetchError, FetchedPage, fetch_url};
use super::provider_health::{self, Capability};
use super::providers::{self, ProviderError, ProviderPage};
use super::snapshot::{self, Snapshot};
use crate::tool::AbortController;
use crate::utils::settings::nyan::WebSettings;

/// Bodies shorter than this look like JS shells or challenge pages.
const MIN_BODY_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchChainFailure {
    pub nyan: String,
    pub attempts: Vec<(String, String)>,
}

impl std::fmt::Display for FetchChainFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = vec![format!("nyan {}", self.nyan)];
        parts.extend(
            self.attempts
                .iter()
                .map(|(provider, error)| format!("{provider} {error}")),
        );
        write!(f, "fetch_failed: {}", parts.join(", "))
    }
}

impl std::error::Error for FetchChainFailure {}

#[derive(Debug)]
pub struct FetchChainResult {
    pub snapshot: Snapshot,
    /// Why the direct fetch was not used; `None` when nyan's page was served.
    pub direct_reason: Option<String>,
    /// `web.fetch_fallback` asked for a provider and every provider failed, so
    /// the page nyan had already fetched was served instead.
    pub challenge_fallback_failed: bool,
}

/// Direct fetch, then provider fallback. `decisions` carries the
/// `web.fetch_fallback` seam; `None` is the rule-only path.
pub async fn fetch_chain(
    raw_url: &str,
    abort_controller: &AbortController,
    web: &WebSettings,
    config: &FetchConfig,
    decisions: Option<&WebDecisions<'_>>,
) -> Result<FetchChainResult, FetchChainFailure> {
    match fetch_url(raw_url, abort_controller, config).await {
        Ok(page) if page.text.chars().count() < MIN_BODY_CHARS => {
            fallback(raw_url, "content_too_short", abort_controller, web).await
        }
        Ok(page) => {
            let challenge = match decisions {
                Some(decisions) => decisions.looks_like_challenge(&page.text).await,
                None => false,
            };
            if challenge {
                return challenge_fallback(raw_url, page, abort_controller, web).await;
            }
            nyan_snapshot(&page)
        }
        Err(error) if should_fallback(&error) => {
            fallback(raw_url, &error.to_string(), abort_controller, web).await
        }
        Err(error) => Err(FetchChainFailure {
            nyan: error.to_string(),
            attempts: Vec::new(),
        }),
    }
}

fn nyan_snapshot(page: &FetchedPage) -> Result<FetchChainResult, FetchChainFailure> {
    let snapshot = snapshot::save_nyan(page).map_err(|error| FetchChainFailure {
        nyan: format!("snapshot_error: {error}"),
        attempts: Vec::new(),
    })?;
    Ok(FetchChainResult {
        snapshot,
        direct_reason: None,
        challenge_fallback_failed: false,
    })
}

/// `web.fetch_fallback` may only add a provider attempt. If none succeeds, the
/// rules had already accepted this page, so it is served rather than failed.
async fn challenge_fallback(
    raw_url: &str,
    page: FetchedPage,
    abort_controller: &AbortController,
    web: &WebSettings,
) -> Result<FetchChainResult, FetchChainFailure> {
    match fallback(raw_url, "challenge_page", abort_controller, web).await {
        Ok(result) => Ok(result),
        Err(failure) => {
            crate::utils::debug::log_for_debugging(&format!(
                "web.fetch_fallback: serving the direct page after provider fallback failed ({failure})"
            ));
            let mut result = nyan_snapshot(&page)?;
            result.challenge_fallback_failed = true;
            Ok(result)
        }
    }
}

async fn fallback(
    raw_url: &str,
    direct_reason: &str,
    abort_controller: &AbortController,
    web: &WebSettings,
) -> Result<FetchChainResult, FetchChainFailure> {
    let started = Instant::now();
    let deadline = Duration::from_millis(web.fetch().fallback_deadline_ms());
    let (providers, build_failures) = providers::configured_providers(web);
    let mut attempts: Vec<(String, String)> = Vec::new();
    for name in web.fallback_order() {
        if abort_controller.is_aborted() {
            attempts.push((name, "cancelled".to_string()));
            break;
        }
        if started.elapsed() >= deadline {
            attempts.push((name, "timeout".to_string()));
            break;
        }
        if let Some(failed) = build_failures.iter().find(|failed| failed.provider == name) {
            attempts.push((name, failed.error.code()));
            continue;
        }
        let Some(provider) = providers
            .iter()
            .find(|provider| provider.as_search().name() == name)
        else {
            continue;
        };
        // Brave / SearXNG have no fetch capability; say so rather than skip.
        let fetch_provider = match provider.as_fetch() {
            Ok(fetch_provider) => fetch_provider,
            Err(error) => {
                attempts.push((name, error.code()));
                continue;
            }
        };
        // A cooling-down provider is skipped before it can spend a session slot
        // (provider-health 3.3 / 3.6). The reason keeps its original code and
        // gains the `(cooling down)` suffix.
        let guard = if provider_health::is_commercial_provider(&name) {
            let fingerprint = provider.fingerprint();
            match provider_health::acquire_permit(&name, Capability::Fetch, Some(&fingerprint)) {
                provider_health::PermitOutcome::Allowed(guard)
                | provider_health::PermitOutcome::Probe(guard) => Some(guard),
                provider_health::PermitOutcome::Skipped { reason, .. } => {
                    attempts.push((name, format!("{reason} (cooling down)")));
                    continue;
                }
            }
        } else {
            None
        };
        let max_calls = web.provider(&name).max_calls_per_session;
        if let Err(error) = budget::reserve_provider(&name, max_calls) {
            crate::utils::debug::log_for_debugging(&format!("web fetch fallback: {error}"));
            attempts.push((name, ProviderError::BudgetExhausted.code()));
            continue;
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        let fetched = tokio::time::timeout(
            remaining,
            fetch_provider.fetch(raw_url.to_string(), remaining),
        )
        .await
        .map_err(|_| ProviderError::Timeout)
        .and_then(|result| result);
        match fetched {
            Ok(page) => {
                if let Some(guard) = guard {
                    guard.record_success();
                }
                let snapshot =
                    save_provider_page(page, name.clone()).map_err(|error| FetchChainFailure {
                        nyan: direct_reason.to_string(),
                        attempts: vec![(name.clone(), format!("snapshot_error: {error}"))],
                    })?;
                return Ok(FetchChainResult {
                    snapshot,
                    direct_reason: Some(direct_reason.to_string()),
                    challenge_fallback_failed: false,
                });
            }
            Err(error) => {
                let code = error.code();
                if let Some(guard) = guard {
                    guard.record_failure(&error);
                }
                attempts.push((name, code));
            }
        }
    }
    if attempts.is_empty() {
        // Distinguish `fetchFallback: []` (explicitly off) from a list whose
        // entries had nothing to try; neither is a silent success.
        let reason = if web.fallback_order().is_empty() {
            "fallback_disabled"
        } else {
            "none_configured"
        };
        attempts.push(("providers".to_string(), reason.to_string()));
    }
    Err(FetchChainFailure {
        nyan: direct_reason.to_string(),
        attempts,
    })
}

fn save_provider_page(page: ProviderPage, provider: String) -> Result<Snapshot, std::io::Error> {
    snapshot::save_provider(snapshot::ProviderSnapshotInput {
        url: page.url.clone(),
        final_url: page.final_url,
        status: page.status,
        content_type: page.content_type,
        fetched_at: Utc::now(),
        fetched_by: provider,
        text: page.content,
        provider_format: page.format,
        freshness: page.freshness,
    })
}

fn should_fallback(error: &FetchError) -> bool {
    matches!(
        error,
        FetchError::HttpStatus(403 | 429 | 451) | FetchError::Timeout
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::services::decision::Answer;
    use crate::services::web::test_support::{
        Route, ScriptedBackend, key_file, only_mocked_providers, serve,
    };

    fn long_page() -> String {
        format!(
            "<html><body><h1>Real page</h1><p>{}</p></body></html>",
            "content ".repeat(60)
        )
    }

    /// `WebSettings` with the given providers pointed at mock base URLs.
    fn web(providers: Value, order: Value) -> WebSettings {
        serde_json::from_value(json!({
            "providers": only_mocked_providers(providers),
            "fetchFallback": order,
            "fetch": {"fallbackDeadlineMs": 3000},
        }))
        .unwrap()
    }

    fn provider_json(origin: &str) -> Value {
        json!({"enabled": true, "baseUrl": origin, "apiKeyFile": key_file()})
    }

    fn test_lock() -> (
        crate::utils::test_env::TestEnvGuard<'static>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let env = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        budget::reset();
        let health = provider_health::test_lock();
        (env, health)
    }

    fn firecrawl_ok() -> Route {
        Route::new(
            200,
            "application/json",
            json!({"data":{"markdown": format!("# From firecrawl\n{}", "provider text ".repeat(30)),
                           "metadata":{"statusCode":200,"contentType":"text/html"}}}).to_string(),
        )
    }

    async fn chain(
        url: &str,
        web: &WebSettings,
        decisions: Option<&WebDecisions<'_>>,
    ) -> Result<FetchChainResult, FetchChainFailure> {
        fetch_chain(
            url,
            &AbortController::default(),
            web,
            &FetchConfig::for_local_mock(),
            decisions,
        )
        .await
    }

    #[test]
    fn only_named_fetch_failures_trigger_provider_fallback() {
        for status in [403, 429, 451] {
            assert!(should_fallback(&FetchError::HttpStatus(status)));
        }
        assert!(should_fallback(&FetchError::Timeout));
        for error in [
            FetchError::HttpStatus(404),
            FetchError::HttpStatus(500),
            FetchError::DeniedPrivateNetwork,
            FetchError::DeniedHost,
            FetchError::DeniedPort(8443),
            FetchError::DeniedScheme,
            FetchError::ProxyUnconfigured,
            FetchError::TooLarge,
            FetchError::TooManyRedirects,
            FetchError::ContentTypeDenied("image/png".into()),
            FetchError::Cancelled,
        ] {
            assert!(!should_fallback(&error), "{error}");
        }
    }

    #[tokio::test]
    async fn a_good_direct_page_never_touches_a_provider() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/ok", Route::html(&long_page()))]).await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        let result = chain(&site.url("/ok"), &settings, None).await.unwrap();
        assert_eq!(result.snapshot.metadata.fetched_by, "nyan");
        assert_eq!(result.direct_reason, None);
        assert!(provider.seen().is_empty());
    }

    #[tokio::test]
    async fn a_403_falls_back_in_order_and_the_first_success_stops_the_chain() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "denied"))]).await;
        let first = serve(vec![(
            "/extract",
            Route::new(500, "application/json", "{}"),
        )])
        .await;
        let second = serve(vec![("/scrape", firecrawl_ok())]).await;
        let third = serve(vec![(
            "/contents",
            Route::new(200, "application/json", "{}"),
        )])
        .await;
        let settings = web(
            json!({
                "anysearch": provider_json(&first.origin),
                "firecrawl": provider_json(&second.origin),
                "exa": provider_json(&third.origin),
            }),
            json!(["anysearch", "firecrawl", "exa"]),
        );
        let result = chain(&site.url("/blocked"), &settings, None).await.unwrap();
        assert_eq!(result.snapshot.metadata.fetched_by, "firecrawl");
        assert_eq!(result.direct_reason.as_deref(), Some("http_403"));
        assert_eq!(
            result.snapshot.metadata.provider_format.as_deref(),
            Some("markdown")
        );
        assert_eq!(first.seen().len(), 1);
        assert_eq!(second.seen().len(), 1);
        assert!(
            third.seen().is_empty(),
            "the chain stops at the first success"
        );
        // The URL is sent as given, with a live-fetch flag.
        let sent: Value = serde_json::from_str(&second.seen()[0].body).unwrap();
        assert_eq!(sent["url"], site.url("/blocked"));
        assert_eq!(sent["maxAge"], 0);
    }

    #[tokio::test]
    async fn timeout_429_451_and_short_bodies_all_fall_back() {
        let (_env, _health) = test_lock();
        let site = serve(vec![
            ("/limited", Route::new(429, "text/html", "slow")),
            ("/legal", Route::new(451, "text/html", "legal")),
            (
                "/slow",
                Route::text("late").delayed(Duration::from_millis(900)),
            ),
            (
                "/shell",
                Route::html("<html><body><div id=app></div></body></html>"),
            ),
        ])
        .await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        let mut config = FetchConfig::for_local_mock();
        config.timeout = Duration::from_millis(200);
        for (path, reason) in [
            ("/limited", "http_429"),
            ("/legal", "http_451"),
            ("/slow", "timeout"),
            ("/shell", "content_too_short"),
        ] {
            let result = fetch_chain(
                &site.url(path),
                &AbortController::default(),
                &settings,
                &config,
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.direct_reason.as_deref(), Some(reason), "{path}");
            assert_eq!(result.snapshot.metadata.fetched_by, "firecrawl", "{path}");
        }
        assert_eq!(provider.seen().len(), 4);
    }

    #[tokio::test]
    async fn ssrf_and_config_denials_never_reach_a_provider() {
        let (_env, _health) = test_lock();
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        // Production guard: no test seam.
        let production = FetchConfig::from_settings(&settings);
        for target in [
            "https://127.0.0.1/",
            "https://localhost./x",
            "https://[::1]/",
            "https://169.254.169.254/latest",
            "https://intranet/",
            "https://example.com:8443/",
            "ftp://example.com/",
        ] {
            let failure = fetch_chain(
                target,
                &AbortController::default(),
                &settings,
                &production,
                None,
            )
            .await
            .unwrap_err();
            assert!(failure.attempts.is_empty(), "{target}: {failure}");
            assert!(
                failure.nyan.starts_with("denied_"),
                "{target}: {}",
                failure.nyan
            );
        }
        // A deny-listed host is a config denial, too.
        let mut denied = FetchConfig::for_local_mock();
        denied.host_guard.deny_hosts = vec!["127.0.0.1".to_string()];
        let site = serve(vec![("/x", Route::new(403, "text/html", "no"))]).await;
        let failure = fetch_chain(
            &site.url("/x"),
            &AbortController::default(),
            &settings,
            &denied,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(failure.nyan, "denied_host");
        assert!(
            provider.seen().is_empty(),
            "no URL may be sent to a provider"
        );
    }

    #[tokio::test]
    async fn proxy_unconfigured_does_not_fall_back() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        let _health_lock = provider_health::test_lock();
        let _guards = ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"]
            .map(crate::utils::test_env::EnvVarGuard::unset);
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        let config = FetchConfig::from_settings(&settings);
        let failure = fetch_chain(
            "https://example.com/",
            &AbortController::default(),
            &settings,
            &config,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(failure.nyan, "proxy_unconfigured");
        assert!(failure.attempts.is_empty());
        assert!(provider.seen().is_empty());
    }

    #[tokio::test]
    async fn non_retryable_http_errors_and_content_types_do_not_fall_back() {
        let (_env, _health) = test_lock();
        let site = serve(vec![
            ("/missing", Route::new(404, "text/html", "gone")),
            ("/boom", Route::new(500, "text/html", "boom")),
            ("/image", Route::new(200, "image/png", vec![1, 2, 3])),
        ])
        .await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        for (path, expected) in [
            ("/missing", "http_404"),
            ("/boom", "http_500"),
            ("/image", "content_type_denied"),
        ] {
            let failure = chain(&site.url(path), &settings, None).await.unwrap_err();
            assert!(
                failure.nyan.starts_with(expected),
                "{path}: {}",
                failure.nyan
            );
        }
        assert!(provider.seen().is_empty());
    }

    #[tokio::test]
    async fn every_failure_is_named_in_order() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "no"))]).await;
        let quota = serve(vec![("/scrape", Route::new(402, "application/json", "{}"))]).await;
        let limited = serve(vec![(
            "/extract",
            Route::new(429, "application/json", "{}"),
        )])
        .await;
        let settings = web(
            json!({
                "firecrawl": provider_json(&quota.origin),
                "anysearch": provider_json(&limited.origin),
                "brave": {"enabled": true, "apiKeyFile": key_file()},
            }),
            json!(["firecrawl", "brave", "anysearch", "exa"]),
        );
        let failure = chain(&site.url("/blocked"), &settings, None)
            .await
            .unwrap_err();
        assert_eq!(failure.nyan, "http_403");
        assert_eq!(
            failure.attempts,
            vec![
                ("firecrawl".to_string(), "quota_exhausted".to_string()),
                ("brave".to_string(), "unsupported".to_string()),
                ("anysearch".to_string(), "rate_limited".to_string()),
            ]
        );
        assert_eq!(
            failure.to_string(),
            "fetch_failed: nyan http_403, firecrawl quota_exhausted, brave unsupported, anysearch rate_limited"
        );
    }

    #[tokio::test]
    async fn no_configured_provider_is_reported_not_silent() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "no"))]).await;
        let settings = web(json!({}), json!(["firecrawl"]));
        let failure = chain(&site.url("/blocked"), &settings, None)
            .await
            .unwrap_err();
        assert_eq!(
            failure.attempts,
            vec![("providers".to_string(), "none_configured".to_string())]
        );
        // `fetchFallback: []` disables the provider step; the failure stays named.
        let settings = web(json!({}), json!([]));
        let failure = chain(&site.url("/blocked"), &settings, None)
            .await
            .unwrap_err();
        assert_eq!(
            failure.attempts,
            vec![("providers".to_string(), "fallback_disabled".to_string())]
        );
        assert_eq!(
            failure.to_string(),
            "fetch_failed: nyan http_403, providers fallback_disabled"
        );
    }

    #[tokio::test]
    async fn a_provider_with_an_unreadable_key_is_named_and_the_next_one_runs() {
        let (_env, _health) = test_lock();
        let dir =
            std::env::temp_dir().join(format!("nyan-fallback-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("empty");
        std::fs::write(&key, "\n").unwrap();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "no"))]).await;
        let good = serve(vec![(
            "/extract",
            Route::new(
                200,
                "application/json",
                json!({"results":[{"url":"u","raw_content":"tavily page ".repeat(40)}]})
                    .to_string(),
            ),
        )])
        .await;
        let settings = web(
            json!({
                "firecrawl": {"enabled": true, "apiKeyFile": key.to_string_lossy()},
                "tavily": provider_json(&good.origin),
            }),
            json!(["firecrawl", "tavily"]),
        );
        let result = chain(&site.url("/blocked"), &settings, None).await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(result.snapshot.metadata.fetched_by, "tavily");
    }

    #[tokio::test]
    async fn provider_session_budget_skips_a_spent_provider() {
        let (_env, _health) = test_lock();
        budget::reset();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "no"))]).await;
        let first = serve(vec![("/scrape", firecrawl_ok())]).await;
        let second = serve(vec![(
            "/extract",
            Route::new(
                200,
                "application/json",
                json!({"content": "anysearch page ".repeat(30)}).to_string(),
            ),
        )])
        .await;
        let mut settings = web(
            json!({
                "firecrawl": {"enabled": true, "baseUrl": first.origin, "apiKeyFile": key_file(), "maxCallsPerSession": 1},
                "anysearch": provider_json(&second.origin),
            }),
            json!(["firecrawl", "anysearch"]),
        );
        settings.fetch_via = None;
        let one = chain(&site.url("/blocked"), &settings, None).await.unwrap();
        assert_eq!(one.snapshot.metadata.fetched_by, "firecrawl");
        // Firecrawl is spent for the session; the next page goes to AnySearch.
        let two = chain(&site.url("/blocked"), &settings, None).await.unwrap();
        assert_eq!(two.snapshot.metadata.fetched_by, "anysearch");
        assert_eq!(first.seen().len(), 1);
        budget::reset();
    }

    #[tokio::test]
    async fn an_aborted_chain_names_the_cancellation_and_sends_nothing() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "no"))]).await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        let abort = AbortController::default();
        abort.abort();
        let failure = fetch_chain(
            &site.url("/blocked"),
            &abort,
            &settings,
            &FetchConfig::for_local_mock(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(failure.nyan, "cancelled");
        assert!(provider.seen().is_empty());
    }

    // ── web.fetch_fallback ─────────────────────────────────────

    fn challenge_answer(
        probability: f64,
    ) -> impl Fn(&crate::services::decision::DecisionRequest) -> std::collections::BTreeMap<String, Answer>
    {
        move |_| {
            [("challenge".to_string(), Answer::Noul { probability })]
                .into_iter()
                .collect()
        }
    }

    #[tokio::test]
    async fn a_confident_challenge_verdict_adds_a_provider_attempt() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/page", Route::html(&long_page()))]).await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        let (decision_provider, backend) = ScriptedBackend::provider(challenge_answer(0.99));
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&decision_provider, &abort);
        let result = chain(&site.url("/page"), &settings, Some(&decisions))
            .await
            .unwrap();
        assert_eq!(result.snapshot.metadata.fetched_by, "firecrawl");
        assert_eq!(result.direct_reason.as_deref(), Some("challenge_page"));
        assert_eq!(backend.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_challenge_fallback_still_serves_the_direct_page() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/page", Route::html(&long_page()))]).await;
        let provider = serve(vec![("/scrape", Route::new(500, "application/json", "{}"))]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        let (decision_provider, _) = ScriptedBackend::provider(challenge_answer(0.99));
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&decision_provider, &abort);
        let result = chain(&site.url("/page"), &settings, Some(&decisions))
            .await
            .unwrap();
        assert_eq!(result.snapshot.metadata.fetched_by, "nyan");
        assert!(result.challenge_fallback_failed);
        assert_eq!(provider.seen().len(), 1, "the provider was tried");
    }

    #[tokio::test]
    async fn a_negative_unsure_or_unavailable_verdict_changes_nothing() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/page", Route::html(&long_page()))]).await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        for probability in [0.01, 0.6] {
            let (decision_provider, _) = ScriptedBackend::provider(challenge_answer(probability));
            let abort = AbortController::default();
            let decisions = WebDecisions::new(&decision_provider, &abort);
            let result = chain(&site.url("/page"), &settings, Some(&decisions))
                .await
                .unwrap();
            assert_eq!(
                result.snapshot.metadata.fetched_by, "nyan",
                "p={probability}"
            );
        }
        let (down, _) =
            ScriptedBackend::failing(crate::services::decision::FallbackReason::BackendDown);
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&down, &abort);
        let result = chain(&site.url("/page"), &settings, Some(&decisions))
            .await
            .unwrap();
        assert_eq!(result.snapshot.metadata.fetched_by, "nyan");
        assert!(provider.seen().is_empty());
    }

    #[tokio::test]
    async fn the_decision_cannot_turn_a_failed_fetch_into_a_success() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/missing", Route::new(404, "text/html", "gone"))]).await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        let (decision_provider, backend) = ScriptedBackend::provider(challenge_answer(0.99));
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&decision_provider, &abort);
        let failure = chain(&site.url("/missing"), &settings, Some(&decisions))
            .await
            .unwrap_err();
        assert_eq!(failure.nyan, "http_404");
        assert!(
            backend.requests().is_empty(),
            "no page, nothing to classify"
        );
        assert!(provider.seen().is_empty());
    }

    // ── Provider health on the fetch path (H3) ─────────────────

    async fn fetch_only(
        site: &crate::services::web::test_support::Server,
        settings: &WebSettings,
    ) -> Result<FetchChainResult, FetchChainFailure> {
        chain(&site.url("/blocked"), settings, None).await
    }

    #[tokio::test]
    async fn acceptance_12_fetch_403_and_404_do_not_cool_down_but_401_does() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "no"))]).await;

        // Fetch 403: page-level, not a key problem → no Down.
        let provider_403 =
            serve(vec![("/scrape", Route::new(403, "application/json", "{}"))]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider_403.origin)}),
            json!(["firecrawl"]),
        );
        let _ = fetch_only(&site, &settings).await.unwrap_err();
        assert!(!provider_health::is_soft_cooling_down_at(
            "firecrawl",
            Capability::Fetch,
            Instant::now()
        ));
        assert!(!provider_health::is_hard_cooling_down_at(
            "firecrawl",
            Capability::Fetch,
            Instant::now()
        ));

        // Fetch 404: request issue → no Down.
        let provider_404 =
            serve(vec![("/scrape", Route::new(404, "application/json", "{}"))]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider_404.origin)}),
            json!(["firecrawl"]),
        );
        let _ = fetch_only(&site, &settings).await.unwrap_err();
        assert!(!provider_health::is_hard_cooling_down_at(
            "firecrawl",
            Capability::Fetch,
            Instant::now()
        ));

        // Fetch 401: account-level → hard Down on both keys.
        let provider_401 =
            serve(vec![("/scrape", Route::new(401, "application/json", "{}"))]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider_401.origin)}),
            json!(["firecrawl"]),
        );
        let _ = fetch_only(&site, &settings).await.unwrap_err();
        assert!(provider_health::is_hard_cooling_down_at(
            "firecrawl",
            Capability::Fetch,
            Instant::now()
        ));
        assert!(provider_health::is_hard_cooling_down_at(
            "firecrawl",
            Capability::Search,
            Instant::now()
        ));
    }

    #[tokio::test]
    async fn acceptance_12_fetch_skips_a_cooling_down_provider_with_the_suffix() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "no"))]).await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        // Quota exhaustion puts the provider into hard Down for Fetch.
        provider_health::record_failure_at(
            "firecrawl",
            Capability::Fetch,
            &ProviderError::QuotaExhausted,
            Instant::now(),
        );
        let failure = fetch_only(&site, &settings).await.unwrap_err();
        assert_eq!(
            failure.attempts,
            vec![(
                "firecrawl".to_string(),
                "quota_exhausted (cooling down)".to_string()
            )]
        );
        assert!(
            provider.seen().is_empty(),
            "a cooling-down provider sends nothing"
        );
    }

    #[tokio::test]
    async fn acceptance_11_fetch_success_does_not_clear_the_search_key() {
        let (_env, _health) = test_lock();
        let site = serve(vec![("/blocked", Route::new(403, "text/html", "no"))]).await;
        let provider = serve(vec![("/scrape", firecrawl_ok())]).await;
        let settings = web(
            json!({"firecrawl": provider_json(&provider.origin)}),
            json!(["firecrawl"]),
        );
        provider_health::record_failure_at(
            "firecrawl",
            Capability::Search,
            &ProviderError::RateLimited,
            Instant::now(),
        );
        assert!(provider_health::is_soft_cooling_down_at(
            "firecrawl",
            Capability::Search,
            Instant::now()
        ));
        let result = fetch_only(&site, &settings).await.unwrap();
        assert_eq!(result.snapshot.metadata.fetched_by, "firecrawl");
        // Fetch succeeded; the Search key is untouched.
        assert!(provider_health::is_soft_cooling_down_at(
            "firecrawl",
            Capability::Search,
            Instant::now()
        ));
        assert!(!provider_health::is_soft_cooling_down_at(
            "firecrawl",
            Capability::Fetch,
            Instant::now()
        ));
    }
}
