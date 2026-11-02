//! Deterministic multi-provider WebSearch aggregation.
//!
//! @nyan: all enabled search providers are queried in parallel. Provider order
//! is never a fallback signal; RRF and byte-order URL ties make the final list
//! reproducible before optional Qwen3 reranking.
//!
//! Pipeline (external-search 3.3): fan-out → normalize + RRF → rerank (optional)
//! → `web.answers` reorder/annotate → domain filter + cap → top N. The Decision
//! Provider can only add parameters, reorder and annotate; it never removes a
//! provider or a result.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use futures::future::join_all;

use crate::services::rerank::{self, RerankClient, RerankKind, RerankRequest};
use crate::services::web::budget;
use crate::services::web::decision::{self, WebDecisions};
use crate::services::web::provider_health::{self, Capability};
use crate::services::web::providers::{self, Hit, ProviderError, SearchRequest};
use crate::tool::AbortController;
use crate::utils::settings::nyan::{WebSearchSettings, WebSettings};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchFailure {
    pub provider: String,
    pub reason: String,
    pub cooling_down: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published_at: Option<String>,
    pub providers: Vec<String>,
    pub provider_count: usize,
    pub rrf_rank: usize,
    /// `web.answers` judged this candidate likely to answer the query.
    pub likely_answers: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    pub failures: Vec<SearchFailure>,
    pub providers_used: Vec<String>,
    pub all_failed: bool,
    pub reranked: bool,
    /// Set only when `web.query_kind` answered; parameters were added, no
    /// provider was skipped.
    pub query_kind: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    NoProviders,
    Budget(String),
    Unavailable(Vec<SearchFailure>),
    Cancelled,
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProviders => f.write_str("unavailable: no web search providers configured"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Budget(message) => f.write_str(message),
            Self::Unavailable(failures) => {
                let details = failures
                    .iter()
                    .map(|failure| {
                        if failure.cooling_down {
                            format!("{} {} (cooling down)", failure.provider, failure.reason)
                        } else {
                            format!("{} {}", failure.provider, failure.reason)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "unavailable: {details}")
            }
        }
    }
}

impl std::error::Error for SearchError {}

/// URLs from the most recent search, best first. Used only to suggest the next
/// candidate in a `web.page_answers` hint; never as a source of truth.
fn recent_candidates() -> &'static std::sync::Mutex<Vec<String>> {
    static CANDIDATES: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> =
        std::sync::OnceLock::new();
    CANDIDATES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// `/clear` starts a new conversation, so old candidates are no longer "next".
pub fn clear_candidates() {
    remember_candidates(Vec::new());
}

pub fn remember_candidates(urls: Vec<String>) {
    if let Ok(mut candidates) = recent_candidates().lock() {
        *candidates = urls;
    }
}

/// The best recent candidate that is not `url`.
pub fn next_candidate(url: &str) -> Option<String> {
    recent_candidates()
        .lock()
        .ok()?
        .iter()
        .find(|candidate| candidate.as_str() != url)
        .cloned()
}

#[allow(clippy::too_many_arguments)]
pub async fn search(
    query: &str,
    allowed_domains: &[String],
    blocked_domains: &[String],
    max_results: usize,
    web: &WebSettings,
    rerank_client: Option<&RerankClient>,
    abort: &AbortController,
) -> Result<SearchOutcome, SearchError> {
    let provider = decision::provider();
    let decisions = provider
        .as_ref()
        .map(|provider| WebDecisions::new(provider, abort));
    search_with(
        query,
        allowed_domains,
        blocked_domains,
        max_results,
        web,
        rerank_client,
        decisions.as_ref(),
        abort,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn search_with(
    query: &str,
    allowed_domains: &[String],
    blocked_domains: &[String],
    max_results: usize,
    web: &WebSettings,
    rerank_client: Option<&RerankClient>,
    decisions: Option<&WebDecisions<'_>>,
    abort: &AbortController,
) -> Result<SearchOutcome, SearchError> {
    let (providers, build_failures) = providers::configured_providers(web);
    if providers.is_empty() {
        // A configured-but-unbuildable provider is a named failure, not
        // "nothing configured" (external-search 3.4).
        if !build_failures.is_empty() {
            return Err(SearchError::Unavailable(
                build_failures
                    .iter()
                    .map(|failure| SearchFailure {
                        provider: failure.provider.clone(),
                        reason: failure.error.code(),
                        cooling_down: false,
                    })
                    .collect(),
            ));
        }
        return Err(SearchError::NoProviders);
    }
    let search_settings = web.search();
    let max_results = if max_results == 0 {
        8
    } else {
        max_results.min(20)
    };
    if abort.is_aborted() {
        return Err(SearchError::Cancelled);
    }
    // The session deadline starts before queueing: time spent waiting for a
    // concurrency slot counts (external-search 3.5).
    let started = Instant::now();
    let total_deadline = Duration::from_millis(search_settings.deadline_ms());
    let _search_permit = budget::begin_search(&search_settings, total_deadline)
        .await
        .map_err(|error| SearchError::Budget(error.to_string()))?;
    let query_kind = match decisions {
        Some(decisions) => decisions.query_kind(query).await,
        None => None,
    };
    if abort.is_aborted() {
        return Err(SearchError::Cancelled);
    }
    let request = SearchRequest {
        query: query.to_string(),
        max_results: search_settings.recall_depth() as usize,
        allowed_domains: allowed_domains.to_vec(),
        blocked_domains: blocked_domains.to_vec(),
        query_kind,
    };

    // Each provider call is bounded by min(per-provider deadline, what is left
    // of the total), so the aggregate cannot outlive the total and a slow
    // provider is a named failure instead of discarding everyone's results
    // (external-search 3.3 / 3.5).
    let per_provider_deadline = Duration::from_millis(search_settings.per_provider_deadline_ms())
        .min(total_deadline.saturating_sub(started.elapsed()));

    // When all six commercial providers are in soft cooldown, release one to try (provider-health 3.3).
    let all_commercial_soft_down =
        provider_health::all_commercial_in_soft_cooldown(Capability::Search);
    let released_commercial = if all_commercial_soft_down {
        providers
            .iter()
            .find(|p| provider_health::is_commercial_provider(p.as_search().name()))
            .map(|p| p.as_search().name().to_string())
    } else {
        None
    };

    let calls = providers.iter().map(|provider| {
        let name = provider.as_search().name().to_string();
        let max_calls = web.provider(&name).max_calls_per_session;
        let request = request.clone();
        let fingerprint = provider.fingerprint();
        let is_released = released_commercial.as_deref() == Some(&name);
        async move {
            let guard = if provider_health::is_commercial_provider(&name) {
                let permit = provider_health::acquire_permit_opt_release(
                    &name,
                    Capability::Search,
                    Some(&fingerprint),
                    is_released,
                );
                match permit {
                    provider_health::PermitOutcome::Allowed(guard) => Some(guard),
                    provider_health::PermitOutcome::Probe(guard) => Some(guard),
                    provider_health::PermitOutcome::Skipped { reason, .. } => {
                        return (name, Err((reason, true)));
                    }
                }
            } else {
                None
            };
            if let Err(error) = budget::reserve_provider(&name, max_calls) {
                crate::utils::debug::log_for_debugging(&format!("web search: {error}"));
                return (name, Err(("budget_exhausted".to_string(), false)));
            }
            let deadline = per_provider_deadline;
            let response =
                tokio::time::timeout(deadline, provider.as_search().search(request, deadline))
                    .await
                    .map_err(|_| ProviderError::Timeout)
                    .and_then(|result| result);
            match response {
                Ok(hits) => {
                    if let Some(guard) = guard {
                        guard.record_success();
                    }
                    (name, Ok(hits))
                }
                Err(error) => {
                    let code = error.code();
                    if let Some(guard) = guard {
                        guard.record_failure(&error);
                    }
                    (name, Err((code, false)))
                }
            }
        }
    });
    let responses = join_all(calls).await;

    let mut providers_used = Vec::new();
    let mut failures = build_failures
        .iter()
        .map(|failure| SearchFailure {
            provider: failure.provider.clone(),
            reason: failure.error.code(),
            cooling_down: false,
        })
        .collect::<Vec<_>>();
    let mut all_hits = Vec::new();
    for (name, result) in responses {
        match result {
            Ok(hits) => {
                providers_used.push(name.clone());
                all_hits.extend(hits.into_iter().map(|hit| (name.clone(), hit)));
            }
            Err((reason, cooling_down)) => failures.push(SearchFailure {
                provider: name,
                reason,
                cooling_down,
            }),
        }
    }
    providers_used.sort();
    failures.sort_by(|left, right| left.provider.cmp(&right.provider));
    if providers_used.is_empty() {
        return Err(SearchError::Unavailable(failures));
    }

    let mut merged = merge_rrf(all_hits, search_settings.rrf_k() as f64);
    merged.truncate(search_settings.fusion_keep() as usize);

    let mut reranked = false;
    if web.search().rerank().enabled() {
        if let Some(client) = rerank_client {
            if rerank_hits(client, query, &mut merged, &web.search()).await {
                reranked = true;
            }
        }
    }

    if let Some(decisions) = decisions {
        // Additive only: flagged candidates move to the front and are marked;
        // nothing is dropped here.
        if let Some(flags) = decisions.answers(query, &merged).await {
            reorder_and_mark(&mut merged, &flags);
        }
        decisions.prune_shadow(query, &merged).await;
    }

    // Domain policy is applied after the decision step (external-search 3.3);
    // it is the only stage that removes candidates.
    merged.retain(|hit| domain_allowed(&hit.url, allowed_domains, blocked_domains));
    let hits = cap_domains(merged, search_settings.domain_cap() as usize, max_results);
    remember_candidates(hits.iter().map(|hit| hit.url.clone()).collect());
    Ok(SearchOutcome {
        hits,
        failures,
        providers_used,
        all_failed: false,
        reranked,
        query_kind: query_kind.map(|kind| match kind {
            providers::QueryKind::Chinese => "chinese",
            providers::QueryKind::TechDocs => "tech_docs",
            providers::QueryKind::News => "news",
            providers::QueryKind::General => "general",
        }),
    })
}

/// Stable reorder: flagged hits first (each group keeps its existing order),
/// and every flagged hit is annotated. The list length never changes.
fn reorder_and_mark(hits: &mut Vec<SearchHit>, flags: &[bool]) {
    for (index, hit) in hits.iter_mut().enumerate() {
        if flags.get(index).copied().unwrap_or(false) {
            hit.likely_answers = true;
        }
    }
    let total = hits.len();
    let mut order = (0..total).collect::<Vec<_>>();
    order.sort_by_key(|index| !flags.get(*index).copied().unwrap_or(false));
    let reordered = order
        .into_iter()
        .map(|index| hits[index].clone())
        .collect::<Vec<_>>();
    *hits = reordered;
}

fn merge_rrf(items: Vec<(String, Hit)>, k: f64) -> Vec<SearchHit> {
    #[derive(Default)]
    struct Aggregate {
        title: String,
        url: String,
        snippet: String,
        published_at: Option<String>,
        providers: BTreeSet<String>,
        score: f64,
    }
    let mut by_url: BTreeMap<String, Aggregate> = BTreeMap::new();
    for (provider, hit) in items {
        let Some(url) = normalize_url(&hit.url) else {
            continue;
        };
        let entry = by_url.entry(url.clone()).or_insert_with(|| Aggregate {
            title: hit.title.clone(),
            url: url.clone(),
            ..Default::default()
        });
        entry.score += 1.0 / (k + hit.provider_rank as f64);
        entry.providers.insert(provider);
        if hit.snippet.chars().count() > entry.snippet.chars().count() {
            entry.snippet = hit.snippet;
        }
        if entry.title.is_empty() {
            entry.title = hit.title;
        }
        if entry.published_at.is_none() {
            entry.published_at = hit.published_at;
        }
    }
    let mut values = by_url.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.url.cmp(&right.url))
    });
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| SearchHit {
            title: value.title,
            url: value.url,
            snippet: value.snippet.chars().take(300).collect(),
            published_at: value.published_at,
            provider_count: value.providers.len(),
            providers: value.providers.into_iter().collect(),
            rrf_rank: index + 1,
            likely_answers: false,
        })
        .collect()
}

async fn rerank_hits(
    client: &RerankClient,
    query: &str,
    hits: &mut [SearchHit],
    settings: &WebSearchSettings,
) -> bool {
    let request = RerankRequest {
        kind: RerankKind::Web,
        query: query.to_string(),
        documents: hits
            .iter()
            .map(|hit| {
                let snippet = hit
                    .snippet
                    .chars()
                    .take(settings.rerank().snippet_chars() as usize)
                    .collect::<String>();
                format!("{}\n{}", hit.title, snippet)
            })
            .collect(),
    };
    let Ok(outcome) = client
        .score(
            &request,
            Duration::from_millis(settings.rerank().timeout_ms()),
        )
        .await
    else {
        // Ranking quality only: keep the RRF order silently (external-search 3.3).
        return false;
    };
    let order = rerank::order_by_score(&outcome.scores, |left, right| {
        hits[left].url.cmp(&hits[right].url)
    });
    let reordered = order
        .into_iter()
        .map(|index| hits[index].clone())
        .collect::<Vec<_>>();
    hits.clone_from_slice(&reordered);
    true
}

fn cap_domains(mut hits: Vec<SearchHit>, cap: usize, max_results: usize) -> Vec<SearchHit> {
    let mut seen = BTreeMap::<String, usize>::new();
    hits.retain(|hit| {
        let domain = reqwest::Url::parse(&hit.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        let count = seen.entry(domain).or_default();
        if *count >= cap {
            false
        } else {
            *count += 1;
            true
        }
    });
    hits.truncate(max_results);
    hits
}

fn domain_allowed(url: &str, allowed: &[String], blocked: &[String]) -> bool {
    let Some(domain) = reqwest::Url::parse(url).ok().and_then(|url| {
        url.host_str()
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
    }) else {
        return false;
    };
    if blocked.iter().any(|entry| host_matches(entry, &domain)) {
        return false;
    }
    allowed.is_empty() || allowed.iter().any(|entry| host_matches(entry, &domain))
}

fn host_matches(entry: &str, domain: &str) -> bool {
    let entry = entry.trim().trim_start_matches('.').to_ascii_lowercase();
    domain == entry || domain.ends_with(&format!(".{entry}"))
}

pub fn normalize_url(raw: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    url.set_fragment(None);
    if url.path().is_empty() {
        url.set_path("/");
    }
    Some(url.to_string())
}

/// The tool-facing rendering (external-search 3.4). Failures are named with
/// their code; a spent provider reads `budget exhausted`.
pub fn render_outcome(outcome: &SearchOutcome) -> String {
    if outcome.hits.is_empty() {
        if outcome.failures.is_empty() {
            return "No results found".to_string();
        }
        return format!(
            "No results found from: {}",
            render_failures(&outcome.failures)
        );
    }
    let mut output = String::new();
    for (index, hit) in outcome.hits.iter().enumerate() {
        output.push_str(&format!("{}. {}\n", index + 1, hit.title));
        output.push_str(&format!(
            "   {} ({} sources: {}{}{})\n",
            hit.url,
            hit.provider_count,
            hit.providers.join(", "),
            hit.published_at
                .as_deref()
                .map(|date| format!(" · published {date}"))
                .unwrap_or_default(),
            if hit.likely_answers {
                " · (likely answers)"
            } else {
                ""
            }
        ));
        if !hit.snippet.is_empty() {
            output.push_str(&format!("   {}\n", hit.snippet));
        }
        output.push('\n');
    }
    if !outcome.providers_used.is_empty() || !outcome.failures.is_empty() {
        output.push_str("[searched: ");
        output.push_str(&outcome.providers_used.join(", "));
        if !outcome.failures.is_empty() {
            output.push_str(" — ");
            output.push_str(&render_failures(&outcome.failures));
        }
        output.push_str("]\n");
    }
    output.push_str("[Search results are leads, not sources. WebFetch a page before citing it.]");
    output
}

fn render_failures(failures: &[SearchFailure]) -> String {
    failures
        .iter()
        .map(|failure| {
            let reason = match failure.reason.as_str() {
                "budget_exhausted" => "budget exhausted".to_string(),
                reason => reason.to_string(),
            };
            if failure.cooling_down {
                format!("{} {reason} (cooling down)", failure.provider)
            } else {
                format!("{} {reason}", failure.provider)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::*;
    use crate::services::decision::{Answer, FallbackReason};
    use crate::services::service_health::{ServiceHealth, ServiceKind};
    use crate::services::web::test_support::{
        Route, ScriptedBackend, key_file, only_mocked_providers, serve,
    };

    fn web(providers: Value, extra: Value) -> WebSettings {
        let mut value = json!({"providers": only_mocked_providers(providers)});
        if let Value::Object(extra) = extra {
            for (key, part) in extra {
                value[key] = part;
            }
        }
        serde_json::from_value(value).unwrap()
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
        crate::services::service_health::shared(
            crate::services::service_health::ServiceKind::Searxng,
        )
        .record_success();
        (env, health)
    }

    fn provider(origin: &str, max_calls: Option<u32>) -> Value {
        let mut value = json!({"enabled": true, "baseUrl": origin, "apiKeyFile": key_file()});
        if let Some(max_calls) = max_calls {
            value["maxCallsPerSession"] = json!(max_calls);
        }
        value
    }

    fn search_route() -> Route {
        Route::new(
            200,
            "application/json",
            json!({"results": [{"title": "A", "url": "https://example.com/a", "content": "snippet"}]})
                .to_string(),
        )
    }

    async fn search_web(
        query: &str,
        web: &WebSettings,
        rerank: Option<&RerankClient>,
        decisions: Option<&WebDecisions<'_>>,
    ) -> Result<SearchOutcome, SearchError> {
        search_with(
            query,
            &[],
            &[],
            8,
            web,
            rerank,
            decisions,
            &AbortController::default(),
        )
        .await
    }

    fn rerank_client(url: &str, health: std::sync::Arc<ServiceHealth>) -> RerankClient {
        crate::utils::tls_provider::install_crypto_provider();
        RerankClient::with_client(
            url,
            "qwen3-reranker-test",
            reqwest::Client::builder().no_proxy().build().unwrap(),
            health,
        )
    }

    fn fresh_health() -> std::sync::Arc<ServiceHealth> {
        std::sync::Arc::new(ServiceHealth::with_timeouts(
            ServiceKind::Rerank,
            Duration::from_millis(300),
            Duration::from_millis(1_000),
        ))
    }

    // ── fan-out ────────────────────────────────────────────────

    #[tokio::test]
    async fn every_enabled_provider_is_queried_with_its_own_cap_and_duplicates_merge() {
        let _lock = test_lock();
        let mut providers = serde_json::Map::new();
        let mut servers = Vec::new();
        // recallDepth defaults to 20; AnySearch's own ceiling is 10.
        let limits = [
            ("anysearch", 10),
            ("brave", 20),
            ("deepseek", 20),
            ("exa", 20),
            ("firecrawl", 20),
            ("tavily", 20),
            ("searxng", 20),
        ];
        for (name, _) in limits {
            let results = (0..30)
                .map(|index| {
                    json!({
                        "title": format!("{name} {index}"),
                        "url": format!("https://example.com/{name}/{index}"),
                        "content": "snippet",
                        "snippet": "snippet",
                        "description": "snippet",
                        "highlights": ["snippet"],
                    })
                })
                .collect::<Vec<_>>();
            let envelope = match name {
                "brave" => json!({"web": {"results": results}}),
                "firecrawl" => json!({"data": {"web": results}}),
                _ => json!({"results": results}),
            };
            let server = serve(vec![(
                "/search",
                Route::new(200, "application/json", envelope.to_string()),
            )])
            .await;
            providers.insert(name.to_string(), provider(&server.origin, None));
            servers.push((name, server));
        }
        let settings = web(Value::Object(providers), json!({}));
        let outcome = search_web("q", &settings, None, None).await.unwrap();
        assert_eq!(outcome.providers_used.len(), 7);
        assert_eq!(
            outcome.providers_used,
            [
                "anysearch",
                "brave",
                "deepseek",
                "exa",
                "firecrawl",
                "searxng",
                "tavily"
            ]
        );
        assert!(outcome.failures.is_empty());
        for (name, cap) in limits {
            let (_, server) = servers
                .iter()
                .find(|(server_name, _)| *server_name == name)
                .unwrap();
            assert_eq!(server.hits("/search"), 1, "{name}");
            let seen = &server.seen()[0];
            let count = match name {
                // SearXNG has no per-request count parameter; the client trims
                // to its ceiling instead.
                "searxng" => {
                    let url =
                        reqwest::Url::parse(&format!("{}{}", server.origin, seen.target)).unwrap();
                    let pairs = url
                        .query_pairs()
                        .map(|(key, value)| (key.into_owned(), value.into_owned()))
                        .collect::<BTreeMap<_, _>>();
                    assert_eq!(pairs.get("format").map(String::as_str), Some("json"));
                    assert!(pairs.get("count").is_none());
                    Some(cap)
                }
                "brave" => {
                    let url =
                        reqwest::Url::parse(&format!("{}{}", server.origin, seen.target)).unwrap();
                    url.query_pairs()
                        .find(|(key, _)| key == "count")
                        .map(|(_, value)| value.parse::<usize>().unwrap())
                }
                _ => {
                    let body: Value = serde_json::from_str(&seen.body).unwrap();
                    let key = match name {
                        "anysearch" | "tavily" => "max_results",
                        "deepseek" => "count",
                        "exa" => "numResults",
                        _ => "limit",
                    };
                    body[key].as_u64().map(|value| value as usize)
                }
            };
            assert_eq!(count, Some(cap), "{name} recall");
        }
        // Domain cap defaults to 3 per host; 7 providers × 30 results collapse.
        assert_eq!(outcome.hits.len(), 3);
        assert!(outcome.hits.iter().all(|hit| hit.provider_count == 1));
    }

    #[tokio::test]
    async fn a_page_hit_by_two_providers_is_marked_with_both() {
        let _lock = test_lock();
        let one = serve(vec![("/search", Route::new(200, "application/json",
            json!({"results":[{"title":"Shared","url":"https://example.com/shared","content":"a"}]}).to_string()))]).await;
        let two = serve(vec![("/search", Route::new(200, "application/json",
            json!({"data":{"web":[{"title":"Shared","url":"https://example.com/shared#frag","description":"b"}]}}).to_string()))]).await;
        let settings = web(
            json!({"exa": provider(&one.origin, None), "firecrawl": provider(&two.origin, None)}),
            json!({}),
        );
        let outcome = search_web("q", &settings, None, None).await.unwrap();
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].url, "https://example.com/shared");
        assert_eq!(outcome.hits[0].provider_count, 2);
        assert_eq!(outcome.hits[0].providers, ["exa", "firecrawl"]);
    }

    // ── partial failure ────────────────────────────────────────

    #[tokio::test]
    async fn one_provider_failing_is_named_and_the_rest_still_return() {
        let _lock = test_lock();
        let ok = serve(vec![("/search", search_route())]).await;
        let broken = serve(vec![("/search", Route::new(402, "application/json", "{}"))]).await;
        let slow = serve(vec![(
            "/search",
            search_route().delayed(Duration::from_millis(800)),
        )])
        .await;
        let limited = serve(vec![("/search", Route::new(429, "application/json", "{}"))]).await;
        let settings = web(
            json!({
                "exa": provider(&ok.origin, None),
                "firecrawl": provider(&broken.origin, None),
                "tavily": provider(&limited.origin, None),
                "searxng": {"enabled": true, "url": slow.origin},
            }),
            json!({"search": {"perProviderDeadlineMs": 300, "deadlineMs": 2000}}),
        );
        let outcome = search_web("q", &settings, None, None).await.unwrap();
        assert_eq!(outcome.providers_used, ["exa"]);
        let failures = outcome
            .failures
            .iter()
            .map(|failure| (failure.provider.as_str(), failure.reason.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            failures,
            [
                ("firecrawl", "quota_exhausted"),
                ("searxng", "timeout"),
                ("tavily", "rate_limited"),
            ]
        );
        let rendered = render_outcome(&outcome);
        assert!(rendered.contains(
            "[searched: exa — firecrawl quota_exhausted, searxng timeout, tavily rate_limited]"
        ));
        assert!(rendered.contains("https://example.com/a"));
    }

    #[tokio::test]
    async fn an_aborted_search_sends_nothing_and_reports_cancelled() {
        let _lock = test_lock();
        let server = serve(vec![("/search", search_route())]).await;
        let settings = web(json!({"exa": provider(&server.origin, None)}), json!({}));
        // The abort check runs before the permit and any provider call.
        let abort = AbortController::default();
        abort.abort();
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            search_with("q", &[], &[], 8, &settings, None, None, &abort),
        )
        .await
        .expect("the abort check must not wait on anything")
        .unwrap_err();
        assert_eq!(error, SearchError::Cancelled);
        assert!(server.seen().is_empty());
        assert_eq!(budget::snapshot().0, 0, "a cancelled search is not charged");
    }

    #[tokio::test]
    async fn queue_time_counts_against_the_total_deadline() {
        let _lock = test_lock();
        let server = serve(vec![("/search", search_route())]).await;
        let settings = web(
            json!({"exa": provider(&server.origin, None)}),
            json!({"search": {"maxConcurrentSearches": 1, "deadlineMs": 300, "perProviderDeadlineMs": 5000}}),
        );
        // Occupy the only concurrency slot for longer than the total deadline.
        let mut held = crate::utils::settings::nyan::WebSearchSettings::default();
        held.max_concurrent_searches = Some(1);
        let permit = budget::begin_search(&held, Duration::from_secs(5))
            .await
            .unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            drop(permit);
        });
        let error = search_web("q", &settings, None, None).await.unwrap_err();
        release.await.unwrap();
        // The queue wait exceeded the 300 ms deadline; the search fails by name.
        assert!(
            matches!(&error, SearchError::Budget(message)
                if message.contains("concurrency deadline")),
            "{error}"
        );
        assert!(server.seen().is_empty());
    }

    #[tokio::test]
    async fn zero_results_from_a_successful_provider_names_the_failed_ones() {
        let _lock = test_lock();
        let empty = serve(vec![(
            "/search",
            Route::new(200, "application/json", json!({"results": []}).to_string()),
        )])
        .await;
        let broken = serve(vec![("/search", Route::new(402, "application/json", "{}"))]).await;
        let settings = web(
            json!({"exa": provider(&empty.origin, None), "firecrawl": provider(&broken.origin, None)}),
            json!({}),
        );
        let outcome = search_web("q", &settings, None, None).await.unwrap();
        assert!(outcome.hits.is_empty());
        assert_eq!(
            render_outcome(&outcome),
            "No results found from: firecrawl quota_exhausted"
        );
    }

    #[test]
    fn a_budget_failure_renders_as_budget_exhausted() {
        let outcome = SearchOutcome {
            hits: Vec::new(),
            failures: vec![SearchFailure {
                provider: "exa".into(),
                reason: "budget_exhausted".into(),
                cooling_down: false,
            }],
            providers_used: Vec::new(),
            all_failed: false,
            reranked: false,
            query_kind: None,
        };
        assert_eq!(
            render_outcome(&outcome),
            "No results found from: exa budget exhausted"
        );
    }

    #[test]
    fn the_next_candidate_skips_the_current_page_and_clear_forgets_all() {
        remember_candidates(vec![
            "https://example.com/a".to_string(),
            "https://example.com/b".to_string(),
        ]);
        assert_eq!(
            next_candidate("https://example.com/a").as_deref(),
            Some("https://example.com/b")
        );
        assert_eq!(
            next_candidate("https://example.com/other").as_deref(),
            Some("https://example.com/a")
        );
        clear_candidates();
        assert_eq!(next_candidate("https://example.com/a"), None);
    }

    #[tokio::test]
    async fn every_provider_failing_is_unavailable_not_empty_results() {
        let _lock = test_lock();
        let auth = serve(vec![("/search", Route::new(401, "application/json", "{}"))]).await;
        let quota = serve(vec![("/search", Route::new(432, "application/json", "{}"))]).await;
        let settings = web(
            json!({
                "exa": provider(&auth.origin, None),
                "tavily": provider(&quota.origin, None),
            }),
            json!({}),
        );
        let error = search_web("q", &settings, None, None).await.unwrap_err();
        assert_eq!(
            error,
            SearchError::Unavailable(vec![
                SearchFailure {
                    provider: "exa".into(),
                    reason: "auth_failed".into(),
                    cooling_down: false,
                },
                SearchFailure {
                    provider: "tavily".into(),
                    reason: "quota_exhausted".into(),
                    cooling_down: false,
                },
            ])
        );
        assert!(
            error
                .to_string()
                .starts_with("unavailable: exa auth_failed")
        );
    }

    #[tokio::test]
    async fn no_providers_and_unbuildable_providers_are_distinguished() {
        let _lock = test_lock();
        let settings = web(json!({}), json!({}));
        assert_eq!(
            search_web("q", &settings, None, None).await.unwrap_err(),
            SearchError::NoProviders
        );
        let settings = web(
            json!({"exa": {"enabled": true, "apiKeyFile": "/nonexistent/dir/key"}}),
            json!({}),
        );
        // Builds, but every call fails named: auth_failed, not "no results".
        let error = search_web("q", &settings, None, None).await.unwrap_err();
        assert_eq!(
            error,
            SearchError::Unavailable(vec![SearchFailure {
                provider: "exa".into(),
                reason: "auth_failed".into(),
                cooling_down: false,
            }])
        );
    }

    // ── budgets ────────────────────────────────────────────────

    #[tokio::test]
    async fn a_spent_provider_is_skipped_and_rendered_as_budget_exhausted() {
        let _lock = test_lock();
        let spent = serve(vec![("/search", search_route())]).await;
        let other = serve(vec![(
            "/search",
            Route::new(
                200,
                "application/json",
                json!({"results":[{"title":"B","url":"https://other.example/b","content":"s"}]})
                    .to_string(),
            ),
        )])
        .await;
        let settings = web(
            json!({
                "exa": provider(&spent.origin, Some(1)),
                "tavily": provider(&other.origin, None),
            }),
            json!({}),
        );
        let first = search_web("q", &settings, None, None).await.unwrap();
        assert!(first.failures.is_empty());
        let second = search_web("q", &settings, None, None).await.unwrap();
        assert_eq!(
            spent.hits("/search"),
            1,
            "the spent provider sent nothing more"
        );
        assert_eq!(
            second.failures,
            vec![SearchFailure {
                provider: "exa".into(),
                reason: "budget_exhausted".into(),
                cooling_down: false,
            }]
        );
        assert!(render_outcome(&second).contains("exa budget exhausted"));
    }

    #[tokio::test]
    async fn the_session_search_cap_stops_requests_with_a_named_error() {
        let _lock = test_lock();
        let server = serve(vec![("/search", search_route())]).await;
        let settings = web(
            json!({"exa": provider(&server.origin, None)}),
            json!({"search": {"maxPerSession": 1}}),
        );
        assert!(search_web("q", &settings, None, None).await.is_ok());
        let error = search_web("q", &settings, None, None).await.unwrap_err();
        assert!(matches!(&error, SearchError::Budget(message)
            if message == "budget_exhausted: web search limit (1 per session) reached"));
        assert_eq!(server.hits("/search"), 1);
    }

    // ── filters and caps ───────────────────────────────────────

    #[tokio::test]
    async fn domain_filters_and_the_domain_cap_apply_after_fusion() {
        let _lock = test_lock();
        let results = vec![
            json!({"title":"A","url":"https://docs.example.com/a","content":"s"}),
            json!({"title":"B","url":"https://docs.example.com/b","content":"s"}),
            json!({"title":"C","url":"https://docs.example.com/c","content":"s"}),
            json!({"title":"D","url":"https://blog.example.com/d","content":"s"}),
            json!({"title":"E","url":"https://blocked.example/e","content":"s"}),
        ];
        let server = serve(vec![(
            "/search",
            Route::new(
                200,
                "application/json",
                json!({"results": results}).to_string(),
            ),
        )])
        .await;
        let settings = web(
            json!({"exa": provider(&server.origin, None)}),
            json!({"search": {"domainCap": 2}}),
        );
        let outcome = search_with(
            "q",
            &[],
            &["blocked.example".to_string()],
            10,
            &settings,
            None,
            None,
            &AbortController::default(),
        )
        .await
        .unwrap();
        let urls = outcome
            .hits
            .iter()
            .map(|hit| hit.url.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            urls,
            [
                "https://docs.example.com/a",
                "https://docs.example.com/b",
                "https://blog.example.com/d"
            ]
        );
        // allowed_domains is an allowlist.
        let outcome = search_with(
            "q",
            &["blog.example.com".to_string()],
            &[],
            10,
            &settings,
            None,
            None,
            &AbortController::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome
                .hits
                .iter()
                .map(|hit| hit.url.as_str())
                .collect::<Vec<_>>(),
            ["https://blog.example.com/d"]
        );
    }

    // ── rerank ─────────────────────────────────────────────────

    #[tokio::test]
    async fn rerank_scores_set_the_final_order_and_ties_use_the_url() {
        let _lock = test_lock();
        let server = serve(vec![(
            "/search",
            Route::new(
                200,
                "application/json",
                json!({"results":[
                    {"title":"A","url":"https://example.com/a","content":"alpha"},
                    {"title":"B","url":"https://example.com/b","content":"beta"}
                ]})
                .to_string(),
            ),
        )])
        .await;
        let rerank = serve(vec![(
            "/score",
            Route::new(
                200,
                "application/json",
                json!({"data":[{"score":0.1},{"score":0.9}]}).to_string(),
            ),
        )])
        .await;
        let settings = web(
            json!({"exa": provider(&server.origin, None)}),
            json!({"search": {"rerank": {"enabled": true}}}),
        );
        let client = rerank_client(&rerank.origin, fresh_health());
        let outcome = search_web("q", &settings, Some(&client), None)
            .await
            .unwrap();
        assert!(outcome.reranked);
        assert_eq!(
            outcome
                .hits
                .iter()
                .map(|hit| hit.url.as_str())
                .collect::<Vec<_>>(),
            ["https://example.com/b", "https://example.com/a"]
        );
        let sent: Value = serde_json::from_str(&rerank.seen()[0].body).unwrap();
        assert!(
            sent["text_1"]
                .as_str()
                .unwrap()
                .contains("<Instruct>: Given a web search query")
        );
        assert_eq!(sent["text_2"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rerank_failure_keeps_the_rrf_order_and_the_service_goes_down() {
        let _lock = test_lock();
        let server = serve(vec![(
            "/search",
            Route::new(
                200,
                "application/json",
                json!({"results":[
                    {"title":"A","url":"https://example.com/a","content":"alpha"},
                    {"title":"B","url":"https://example.com/b","content":"beta"}
                ]})
                .to_string(),
            ),
        )])
        .await;
        let rerank = serve(vec![("/score", Route::new(500, "application/json", "{}"))]).await;
        let settings = web(
            json!({"exa": provider(&server.origin, None)}),
            json!({"search": {"rerank": {"enabled": true, "timeoutMs": 500}}}),
        );
        let health = fresh_health();
        let client = rerank_client(&rerank.origin, std::sync::Arc::clone(&health));
        let outcome = search_web("q", &settings, Some(&client), None)
            .await
            .unwrap();
        assert!(!outcome.reranked);
        assert!(
            outcome.failures.is_empty(),
            "rerank is not a provider failure"
        );
        assert_eq!(
            outcome
                .hits
                .iter()
                .map(|hit| hit.url.as_str())
                .collect::<Vec<_>>(),
            ["https://example.com/a", "https://example.com/b"]
        );
        assert!(health.is_down());
        // While Down the next search is not even sent.
        let outcome = search_web("q", &settings, Some(&client), None)
            .await
            .unwrap();
        assert!(!outcome.reranked);
        assert_eq!(rerank.hits("/score"), 1);
    }

    // ── Decision Provider ──────────────────────────────────────

    fn flags(
        answer: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> impl Fn(&crate::services::decision::DecisionRequest) -> BTreeMap<String, Answer> {
        move |request| {
            request
                .questions
                .keys()
                .map(|id| {
                    let probability = if answer(id) { 0.97 } else { 0.03 };
                    (id.clone(), Answer::Noul { probability })
                })
                .collect()
        }
    }

    #[tokio::test]
    async fn web_answers_moves_flagged_candidates_to_the_front_without_dropping_any() {
        let _lock = test_lock();
        let server = serve(vec![(
            "/search",
            Route::new(
                200,
                "application/json",
                json!({"results":[
                    {"title":"A","url":"https://example.com/a","content":"alpha"},
                    {"title":"B","url":"https://example.com/b","content":"beta"},
                    {"title":"C","url":"https://example.com/c","content":"gamma"}
                ]})
                .to_string(),
            ),
        )])
        .await;
        let settings = web(
            json!({"exa": provider(&server.origin, None)}),
            json!({"search": {"domainCap": 9}}),
        );
        let (provider, backend) = ScriptedBackend::provider(flags(|id| id == "c2"));
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&provider, &abort);
        let outcome = search_web("q", &settings, None, Some(&decisions))
            .await
            .unwrap();
        assert_eq!(outcome.hits.len(), 3, "no candidate is removed");
        assert_eq!(
            outcome
                .hits
                .iter()
                .map(|hit| hit.url.as_str())
                .collect::<Vec<_>>(),
            [
                "https://example.com/c",
                "https://example.com/a",
                "https://example.com/b"
            ]
        );
        assert!(outcome.hits[0].likely_answers);
        assert!(!outcome.hits[1].likely_answers);
        assert!(render_outcome(&outcome).contains("likely answers"));
        // query_kind → answers → shadow prune; exactly one answers request.
        let requests = backend.requests();
        let answers = requests
            .iter()
            .filter(|request| request.point == crate::services::decision::DecisionPoint::WebAnswers)
            .collect::<Vec<_>>();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].questions.len(), 3, "one question per candidate");
    }

    #[tokio::test]
    async fn web_answers_unavailable_leaves_the_order_untouched() {
        let _lock = test_lock();
        let server = serve(vec![(
            "/search",
            Route::new(
                200,
                "application/json",
                json!({"results":[
                    {"title":"A","url":"https://example.com/a","content":"alpha"},
                    {"title":"B","url":"https://example.com/b","content":"beta"}
                ]})
                .to_string(),
            ),
        )])
        .await;
        let settings = web(json!({"exa": provider(&server.origin, None)}), json!({}));
        let (provider, _) = ScriptedBackend::failing(FallbackReason::Timeout);
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&provider, &abort);
        let outcome = search_web("q", &settings, None, Some(&decisions))
            .await
            .unwrap();
        assert_eq!(
            outcome
                .hits
                .iter()
                .map(|hit| hit.url.as_str())
                .collect::<Vec<_>>(),
            ["https://example.com/a", "https://example.com/b"]
        );
        assert!(outcome.hits.iter().all(|hit| !hit.likely_answers));
    }

    #[tokio::test]
    async fn web_query_kind_adds_provider_parameters_and_keeps_all_providers() {
        let _lock = test_lock();
        let any = serve(vec![("/search", search_route())]).await;
        let exa = serve(vec![("/search", search_route())]).await;
        let settings = web(
            json!({"anysearch": provider(&any.origin, None), "exa": provider(&exa.origin, None)}),
            json!({}),
        );
        let (provider, _) = ScriptedBackend::provider(|request| match request.point {
            crate::services::decision::DecisionPoint::WebQueryKind => [(
                "kind".to_string(),
                Answer::Choice {
                    choice: "chinese".to_string(),
                    probabilities: BTreeMap::new(),
                    confidence: 0.95,
                },
            )]
            .into_iter()
            .collect(),
            _ => request
                .questions
                .keys()
                .map(|id| (id.clone(), Answer::Noul { probability: 0.03 }))
                .collect(),
        });
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&provider, &abort);
        let outcome = search_web("大模型 压缩", &settings, None, Some(&decisions))
            .await
            .unwrap();
        assert_eq!(outcome.query_kind, Some("chinese"));
        assert_eq!(
            outcome.providers_used,
            ["anysearch", "exa"],
            "no provider was removed"
        );
        let body: Value = serde_json::from_str(&any.seen()[0].body).unwrap();
        assert_eq!(body["zone"], "cn");
        let exa_body: Value = serde_json::from_str(&exa.seen()[0].body).unwrap();
        assert_eq!(
            exa_body["query"], "大模型 压缩",
            "providers without a zone parameter are unchanged"
        );
    }

    #[tokio::test]
    async fn web_prune_stays_shadow_and_never_changes_the_results() {
        let _lock = test_lock();
        let server = serve(vec![(
            "/search",
            Route::new(
                200,
                "application/json",
                json!({"results":[
                    {"title":"A","url":"https://example.com/a","content":"alpha"},
                    {"title":"B","url":"https://example.com/b","content":"beta"},
                    {"title":"C","url":"https://example.com/c","content":"gamma"},
                    {"title":"D","url":"https://example.com/d","content":"delta"},
                    {"title":"E","url":"https://example.com/e","content":"epsilon"},
                    {"title":"F","url":"https://example.com/f","content":"zeta"}
                ]})
                .to_string(),
            ),
        )])
        .await;
        let settings = web(
            json!({"exa": provider(&server.origin, None)}),
            json!({"search": {"domainCap": 9}}),
        );
        let (provider, backend) = ScriptedBackend::provider(flags(|_| false));
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&provider, &abort);
        let outcome = search_web("q", &settings, None, Some(&decisions))
            .await
            .unwrap();
        assert_eq!(outcome.hits.len(), 6, "shadow prune removes nothing");
        let points = backend
            .requests()
            .iter()
            .map(|request| request.point)
            .collect::<Vec<_>>();
        assert!(points.contains(&crate::services::decision::DecisionPoint::WebPrune));
        assert!(points.contains(&crate::services::decision::DecisionPoint::WebAnswers));
    }

    // ── pure helpers ───────────────────────────────────────────

    #[test]
    fn url_normalization_removes_fragments_and_adds_root() {
        assert_eq!(
            normalize_url("HTTPS://Example.com#part").as_deref(),
            Some("https://example.com/")
        );
        assert!(normalize_url("file:///tmp/a").is_none());
    }

    #[test]
    fn rrf_merges_sources_and_uses_url_tie_break() {
        let hits = merge_rrf(
            vec![
                (
                    "brave".to_string(),
                    Hit {
                        title: "B".to_string(),
                        url: "https://example.com/b".to_string(),
                        snippet: "b".to_string(),
                        published_at: None,
                        provider_rank: 1,
                    },
                ),
                (
                    "exa".to_string(),
                    Hit {
                        title: "A".to_string(),
                        url: "https://example.com/a".to_string(),
                        snippet: "a".to_string(),
                        published_at: None,
                        provider_rank: 1,
                    },
                ),
                (
                    "deepseek".to_string(),
                    Hit {
                        title: "B".to_string(),
                        url: "https://example.com/b#frag".to_string(),
                        snippet: "longer".to_string(),
                        published_at: None,
                        provider_rank: 2,
                    },
                ),
            ],
            60.0,
        );
        assert_eq!(hits[0].url, "https://example.com/b");
        assert_eq!(hits[0].provider_count, 2);
        assert_eq!(hits[0].snippet, "longer");
    }

    #[test]
    fn domain_cap_is_deterministic() {
        let hits = (0..4)
            .map(|index| SearchHit {
                title: index.to_string(),
                url: format!("https://same.example/{index}"),
                snippet: String::new(),
                published_at: None,
                providers: vec!["x".to_string()],
                provider_count: 1,
                rrf_rank: index,
                likely_answers: false,
            })
            .collect();
        assert_eq!(cap_domains(hits, 2, 8).len(), 2);
    }

    #[test]
    fn reorder_is_stable_and_marks_only_flagged_hits() {
        let mut hits = (0..4)
            .map(|index| SearchHit {
                title: index.to_string(),
                url: format!("https://example.com/{index}"),
                snippet: String::new(),
                published_at: None,
                providers: vec!["x".to_string()],
                provider_count: 1,
                rrf_rank: index,
                likely_answers: false,
            })
            .collect::<Vec<_>>();
        reorder_and_mark(&mut hits, &[false, true, false, true]);
        assert_eq!(
            hits.iter()
                .map(|hit| hit.title.as_str())
                .collect::<Vec<_>>(),
            ["1", "3", "0", "2"]
        );
        assert_eq!(
            hits.iter()
                .map(|hit| hit.likely_answers)
                .collect::<Vec<_>>(),
            [true, true, false, false]
        );
    }

    // ── Commercial Provider Health (H2) ────────────────────────

    #[tokio::test]
    async fn acceptance_1_soft_failure_timeout_enters_down_on_consecutive_two_and_resets_on_success()
     {
        let _lock = test_lock();
        let timeout_server =
            serve(vec![("/search", Route::new(500, "application/json", "{}"))]).await;
        let settings = web(
            json!({"exa": provider(&timeout_server.origin, None)}),
            json!({}),
        );

        // Search 1: 1st 500 failure -> soft streak = 1, not Down
        let err1 = search_web("q", &settings, None, None).await.unwrap_err();
        assert!(matches!(err1, SearchError::Unavailable(_)));
        assert_eq!(timeout_server.hits("/search"), 1);
        assert!(!provider_health::is_soft_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        // Search 2: 2nd consecutive 500 failure -> enters Down!
        let err2 = search_web("q", &settings, None, None).await.unwrap_err();
        assert!(matches!(err2, SearchError::Unavailable(_)));
        assert_eq!(timeout_server.hits("/search"), 2);
        assert!(provider_health::is_soft_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        // Search 3: while Down, 0 requests sent, returns cooling down
        let err3 = search_web("q", &settings, None, None).await.unwrap_err();
        assert_eq!(
            timeout_server.hits("/search"),
            2,
            "down provider receives 0 requests"
        );
        match err3 {
            SearchError::Unavailable(failures) => {
                assert_eq!(failures.len(), 1);
                assert_eq!(failures[0].provider, "exa");
                assert_eq!(failures[0].reason, "http_500");
                assert!(failures[0].cooling_down);
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }

        // Now test that a success in between resets streak
        provider_health::reset();
        let fail_server = serve(vec![("/search", Route::new(500, "application/json", "{}"))]).await;
        let success_server = serve(vec![("/search", search_route())]).await;
        let fail_settings = web(
            json!({"exa": provider(&fail_server.origin, None)}),
            json!({}),
        );
        let success_settings = web(
            json!({"exa": provider(&success_server.origin, None)}),
            json!({}),
        );

        // 1st failure (streak = 1)
        let _ = search_web("q", &fail_settings, None, None).await;
        assert_eq!(
            provider_health::shared().soft_streak("exa", Capability::Search),
            1
        );
        assert!(!provider_health::is_soft_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        // 2nd call succeeds (resets streak = 0)
        let _ = search_web("q", &success_settings, None, None).await;
        assert_eq!(
            provider_health::shared().soft_streak("exa", Capability::Search),
            0
        );
        assert!(!provider_health::is_soft_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        // 3rd call fails (streak = 1 again, not Down)
        let _ = search_web("q", &fail_settings, None, None).await;
        assert_eq!(
            provider_health::shared().soft_streak("exa", Capability::Search),
            1
        );
        assert!(!provider_health::is_soft_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));
        assert_eq!(fail_server.hits("/search"), 2);
        assert_eq!(success_server.hits("/search"), 1);
    }

    #[tokio::test]
    async fn acceptance_2_rate_limit_enters_down_on_first_failure() {
        let _lock = test_lock();
        let server = serve(vec![("/search", Route::new(429, "application/json", "{}"))]).await;
        let settings = web(json!({"brave": provider(&server.origin, None)}), json!({}));

        // 1st 429 enters Down immediately
        let _ = search_web("q", &settings, None, None).await;
        assert_eq!(server.hits("/search"), 1);
        assert!(provider_health::is_soft_cooling_down_at(
            "brave",
            Capability::Search,
            Instant::now()
        ));

        // 2nd search is skipped
        let err = search_web("q", &settings, None, None).await.unwrap_err();
        assert_eq!(server.hits("/search"), 1, "0 requests in cooldown");
        match err {
            SearchError::Unavailable(failures) => {
                assert_eq!(failures[0].provider, "brave");
                assert_eq!(failures[0].reason, "rate_limited");
                assert!(failures[0].cooling_down);
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn acceptance_3_quota_exhausted_enters_down_immediately_and_renders_suffix() {
        let _lock = test_lock();
        let server = serve(vec![("/search", Route::new(432, "application/json", "{}"))]).await;
        let settings = web(json!({"tavily": provider(&server.origin, None)}), json!({}));

        // 1st quota failure enters hard Down
        let _ = search_web("q", &settings, None, None).await;
        assert_eq!(server.hits("/search"), 1);
        assert!(provider_health::is_hard_cooling_down_at(
            "tavily",
            Capability::Search,
            Instant::now()
        ));

        // Next call is skipped, 0 requests sent
        let err = search_web("q", &settings, None, None).await.unwrap_err();
        assert_eq!(server.hits("/search"), 1);
        assert_eq!(
            err.to_string(),
            "unavailable: tavily quota_exhausted (cooling down)"
        );
    }

    #[tokio::test]
    async fn acceptance_6_outer_timeout_records_failure_at_call_site() {
        let _lock = test_lock();
        // Server hangs for 500ms, perProviderDeadlineMs is 50ms
        let mut delayed_route = Route::new(200, "application/json", "{}");
        delayed_route.delay = Duration::from_millis(500);
        let server = serve(vec![("/search", delayed_route)]).await;
        let settings = web(
            json!({"exa": provider(&server.origin, None)}),
            json!({"search": {"perProviderDeadlineMs": 50, "deadlineMs": 500}}),
        );

        let err = search_web("q", &settings, None, None).await.unwrap_err();
        assert!(matches!(err, SearchError::Unavailable(_)));
        // Outer timeout fired, and health failure was recorded outside the timeout!
        assert_eq!(
            provider_health::shared().soft_streak("exa", Capability::Search),
            1
        );
    }

    #[tokio::test]
    async fn acceptance_8_all_commercial_in_soft_down_releases_one_and_all_hard_down_skips_all() {
        let _lock = test_lock();

        // 1. Put all 6 commercial providers into soft Down
        let now = Instant::now();
        for name in provider_health::COMMERCIAL_PROVIDERS {
            provider_health::record_failure_at(
                name,
                Capability::Search,
                &ProviderError::RateLimited,
                now,
            );
            assert!(provider_health::is_soft_cooling_down_at(
                name,
                Capability::Search,
                now
            ));
        }
        assert!(provider_health::all_commercial_in_soft_cooldown(
            Capability::Search
        ));

        // SearXNG is ready and commercial providers are all soft down
        let searxng_server = serve(vec![("/search", Route::new(200, "application/json",
            json!({"results": [{"title": "SearXNG Hit", "url": "https://example.com/sx", "content": "snippet"}]}).to_string()))]).await;
        let commercial_server = serve(vec![("/search", search_route())]).await;

        let mut providers_map = serde_json::Map::new();
        providers_map.insert(
            "searxng".to_string(),
            json!({"enabled": true, "url": searxng_server.origin}),
        );
        for name in provider_health::COMMERCIAL_PROVIDERS {
            providers_map.insert(name.to_string(), provider(&commercial_server.origin, None));
        }
        let settings = web(Value::Object(providers_map), json!({}));

        let outcome = search_web("q", &settings, None, None).await.unwrap();
        // SearXNG succeeded
        assert!(outcome.providers_used.contains(&"searxng".to_string()));
        // Exactly ONE commercial provider was released and probed (e.g. anysearch, the first in order)
        let commercial_used = outcome
            .providers_used
            .iter()
            .filter(|p| provider_health::is_commercial_provider(p))
            .count();
        assert_eq!(
            commercial_used, 1,
            "exactly one commercial provider is released when all are soft down"
        );

        // 2. Put all commercial providers into HARD Down
        provider_health::reset();
        let now = Instant::now();
        for name in provider_health::COMMERCIAL_PROVIDERS {
            provider_health::record_failure_at(
                name,
                Capability::Search,
                &ProviderError::QuotaExhausted,
                now,
            );
            assert!(provider_health::is_hard_cooling_down_at(
                name,
                Capability::Search,
                now
            ));
        }

        // When all are in hard Down, NO commercial provider is released!
        let outcome2 = search_web("q", &settings, None, None).await.unwrap();
        let commercial_used2 = outcome2
            .providers_used
            .iter()
            .filter(|p| provider_health::is_commercial_provider(p))
            .count();
        assert_eq!(
            commercial_used2, 0,
            "no commercial provider is released when in hard Down"
        );
    }

    #[tokio::test]
    async fn acceptance_10_skipped_calls_do_not_consume_budget_but_failed_calls_do() {
        let _lock = test_lock();

        let server = serve(vec![("/search", Route::new(500, "application/json", "{}"))]).await;
        let settings = web(json!({"exa": provider(&server.origin, Some(5))}), json!({}));

        // Put exa into Down
        provider_health::record_failure_at(
            "exa",
            Capability::Search,
            &ProviderError::RateLimited,
            Instant::now(),
        );
        assert!(provider_health::is_soft_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        let initial_calls = budget::snapshot().3.get("exa").copied().unwrap_or(0);
        assert_eq!(initial_calls, 0);

        // Call search: exa is skipped
        let _ = search_web("q", &settings, None, None).await;
        let skipped_calls = budget::snapshot().3.get("exa").copied().unwrap_or(0);
        assert_eq!(skipped_calls, 0, "skipped calls do not increment budget");

        // Clear health: now exa is attempted, fails with 500
        provider_health::reset();
        let _ = search_web("q", &settings, None, None).await;
        let executed_calls = budget::snapshot().3.get("exa").copied().unwrap_or(0);
        assert_eq!(
            executed_calls, 1,
            "executed failed calls do increment budget"
        );
    }

    #[tokio::test]
    async fn acceptance_7_key_change_clears_cooldown_immediately() {
        let _lock = test_lock();
        let server = serve(vec![("/search", search_route())]).await;
        let dir =
            std::env::temp_dir().join(format!("nyan-health-key-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("key");
        std::fs::write(&key_path, "first-key\n").unwrap();

        let mut provider_json = provider(&server.origin, None);
        provider_json["apiKeyFile"] = json!(key_path.to_string_lossy());
        let settings = web(json!({"exa": provider_json}), json!({}));

        // Put exa into a hard Down window without waiting for it.
        provider_health::record_failure_at(
            "exa",
            Capability::Search,
            &ProviderError::QuotaExhausted,
            Instant::now(),
        );
        assert!(provider_health::is_hard_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        // Inside the window the provider is skipped: no request.
        let _ = search_web("q", &settings, None, None).await;
        assert_eq!(server.hits("/search"), 0);

        // Rewrite the key file: the next search goes through immediately.
        std::fs::write(&key_path, "second-key\n").unwrap();
        let outcome = search_web("q", &settings, None, None).await.unwrap();
        assert_eq!(outcome.providers_used, ["exa"]);
        assert_eq!(server.hits("/search"), 1);
        assert!(!provider_health::is_hard_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        // The key itself never appears in the fingerprint.
        let fingerprint = provider_health::compute_fingerprint(&server.origin, Some("second-key"));
        assert!(!fingerprint.contains("second-key"));
        assert_eq!(fingerprint.len(), 16);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn acceptance_13_searxng_not_in_provider_health_registry() {
        let _lock = test_lock();

        let sx_server = serve(vec![("/search", Route::new(500, "application/json", "{}"))]).await;
        let settings = web(
            json!({"searxng": {"enabled": true, "url": sx_server.origin}}),
            json!({}),
        );

        let _ = search_web("q", &settings, None, None).await;
        // Provider health registry must not track searxng
        assert!(!provider_health::is_soft_cooling_down_at(
            "searxng",
            Capability::Search,
            Instant::now()
        ));
        assert_eq!(
            provider_health::shared().downs("searxng", Capability::Search),
            0
        );
    }

    #[tokio::test]
    async fn clear_resets_provider_health_cooldowns() {
        let _lock = test_lock();

        let server = serve(vec![("/search", search_route())]).await;
        let settings = web(json!({"exa": provider(&server.origin, None)}), json!({}));

        // Put exa in Down
        provider_health::record_failure_at(
            "exa",
            Capability::Search,
            &ProviderError::QuotaExhausted,
            Instant::now(),
        );
        assert!(provider_health::is_hard_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        // /clear resets provider health!
        crate::commands::clear::caches::clear_session_caches(&std::collections::HashSet::new());
        assert!(!provider_health::is_hard_cooling_down_at(
            "exa",
            Capability::Search,
            Instant::now()
        ));

        // Immediately available
        let outcome = search_web("q", &settings, None, None).await.unwrap();
        assert_eq!(outcome.providers_used, ["exa"]);
    }
}
