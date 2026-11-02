//! Provider wire layer: request bodies, auth, deadlines, SearXNG health.
//!
//! @nyan: request construction is split from sending so each provider's body
//! and URL can be asserted without a socket ([`search_call`], [`fetch_call`]).

use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use futures::StreamExt;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::{ProviderError, ProviderKind, QueryKind, SearchRequest, errors, read_key};
use crate::services::service_health::{self, CallPermit, FailureKind, ServiceHealth, ServiceKind};
use crate::services::web::provider_health::Capability;
use crate::utils::settings::nyan::ResolvedProvider;

/// Provider responses are JSON search results or one converted page; anything
/// beyond this is not a response we want to buffer.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// `web.query_kind = news` asks time-aware parameters for roughly a month.
const NEWS_WINDOW_DAYS: i64 = 31;

#[derive(Clone)]
pub struct ProviderClient {
    kind: ProviderKind,
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
    health: Option<Arc<ServiceHealth>>,
}

/// One outgoing provider request, before auth headers.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Call {
    Get(Url),
    Post(Url, Value),
}

impl ProviderClient {
    pub fn from_resolved(
        kind: ProviderKind,
        resolved: &ResolvedProvider,
    ) -> Result<Self, ProviderError> {
        let base_url = resolved
            .base_url
            .as_deref()
            .or(resolved.url.as_deref())
            .unwrap_or(kind.default_base_url())
            .to_string();
        let api_key = read_key(resolved.api_key_file.as_deref())?;
        // SearXNG's availability is process state shared by every search.
        let health =
            (kind == ProviderKind::Searxng).then(|| service_health::shared(ServiceKind::Searxng));
        Self::new(kind, base_url, api_key, health)
    }

    /// Test seam. SearXNG gets a private health machine so a mock outage cannot
    /// leak into the process-wide one.
    pub fn with_base_url(
        kind: ProviderKind,
        base_url: String,
        api_key: Option<String>,
    ) -> Result<Self, ProviderError> {
        let health = (kind == ProviderKind::Searxng)
            .then(|| Arc::new(ServiceHealth::new(ServiceKind::Searxng)));
        Self::new(kind, base_url, api_key, health)
    }

    pub fn fingerprint(&self) -> String {
        crate::services::web::provider_health::compute_fingerprint(
            &self.base_url,
            self.api_key.as_deref(),
        )
    }

    fn new(
        kind: ProviderKind,
        base_url: String,
        api_key: Option<String>,
        health: Option<Arc<ServiceHealth>>,
    ) -> Result<Self, ProviderError> {
        crate::utils::tls_provider::install_crypto_provider();
        let parsed = Url::parse(&base_url).map_err(|_| ProviderError::Invalid)?;
        // SearXNG is a tailnet address; loopback never goes through a proxy.
        let direct = kind == ProviderKind::Searxng || is_loopback(&parsed);
        let builder = if direct {
            crate::utils::proxy::client_builder().no_proxy()
        } else {
            crate::utils::proxy::create_axios_instance().map_err(|_| ProviderError::Network)?
        };
        let http = builder.build().map_err(|_| ProviderError::Network)?;
        let client = Self {
            kind,
            base_url,
            api_key,
            http,
            health,
        };
        client.install_probe();
        Ok(client)
    }

    /// Same check Uptime Kuma runs: a search for `test` returns at least one
    /// result (external-search 3.2.1).
    fn install_probe(&self) {
        let (Some(health), ProviderKind::Searxng) = (&self.health, self.kind) else {
            return;
        };
        let http = self.http.clone();
        let Ok(url) = endpoint(&self.base_url, "search").map(|mut url| {
            url.query_pairs_mut()
                .append_pair("q", "test")
                .append_pair("format", "json");
            url
        }) else {
            return;
        };
        health.set_probe(Some(Arc::new(move || {
            let http = http.clone();
            let url = url.clone();
            Box::pin(async move {
                let response = http.get(url).send().await.map_err(|_| ())?;
                if !response.status().is_success() {
                    return Err(());
                }
                let body: Value = response.json().await.map_err(|_| ())?;
                let has_result = body
                    .get("results")
                    .and_then(Value::as_array)
                    .is_some_and(|results| !results.is_empty());
                if has_result { Ok(()) } else { Err(()) }
            })
        })));
    }

    pub async fn search(
        &self,
        request: &SearchRequest,
        deadline: Duration,
    ) -> Result<Value, ProviderError> {
        if let Some(health) = &self.health {
            match health.call_permit() {
                CallPermit::Skipped => return Err(ProviderError::Unavailable),
                CallPermit::Probe => {
                    health.spawn_probe();
                    return Err(ProviderError::Unavailable);
                }
                CallPermit::Allowed => {}
            }
        }
        let result = match search_call(self.kind, &self.base_url, request, Utc::now().date_naive())
        {
            Ok(call) => self.send(call, deadline, Capability::Search).await,
            Err(error) => Err(error),
        };
        if let Some(health) = &self.health {
            match &result {
                Ok(_) => health.record_success(),
                Err(error) => health.record_failure(failure_kind(error)),
            }
        }
        result
    }

    pub async fn fetch(&self, url: &str, deadline: Duration) -> Result<Value, ProviderError> {
        let call = fetch_call(self.kind, &self.base_url, url)?;
        self.send(call, deadline, Capability::Fetch).await
    }

    async fn send(
        &self,
        call: Call,
        deadline: Duration,
        capability: Capability,
    ) -> Result<Value, ProviderError> {
        // A provider that needs a key is never called without one: the query
        // would leave the machine for a guaranteed 401. AnySearch also serves
        // anonymous callers, so it is the exception.
        if self.kind.requires_key()
            && self.kind != ProviderKind::AnySearch
            && self.api_key.is_none()
        {
            return Err(ProviderError::Auth);
        }
        let mut request = match call {
            Call::Get(url) => self.http.request(Method::GET, url),
            Call::Post(url, body) => self.http.request(Method::POST, url).json(&body),
        }
        .header(reqwest::header::ACCEPT, "application/json");
        if let Some(key) = self.api_key.as_deref() {
            request = match self.kind {
                ProviderKind::Brave => request.header("X-Subscription-Token", key),
                ProviderKind::Exa => request.header("x-api-key", key),
                ProviderKind::Searxng => request,
                _ => request.bearer_auth(key),
            };
        }
        // One wall-clock bound for connect, headers and body.
        tokio::time::timeout(deadline, self.exchange(request, capability))
            .await
            .map_err(|_| ProviderError::Timeout)?
    }

    async fn exchange(
        &self,
        request: reqwest::RequestBuilder,
        capability: Capability,
    ) -> Result<Value, ProviderError> {
        let response = request.send().await.map_err(transport_error)?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = read_capped(response).await?;
        if !status.is_success() {
            errors::log_failure(self.kind, status.as_u16(), &body, self.api_key.as_deref());
            // On the fetch path a 403 means the target page is unavailable (blocked
            // or forbidden), not a key problem; keep the raw status so provider
            // health does not drop a whole provider for one unreadable page
            // (provider-health 3.6). Search keeps the `Auth` mapping in `errors`.
            if capability == Capability::Fetch && status.as_u16() == 403 {
                return Err(ProviderError::Http(403));
            }
            return Err(errors::classify(
                self.kind,
                status.as_u16(),
                &headers,
                &body,
            ));
        }
        // Brave documents the monthly window as the second value; a spent
        // month is a quota failure even when the response carried results.
        if self.kind == ProviderKind::Brave && errors::brave_monthly_exhausted(&headers) {
            return Err(ProviderError::QuotaExhausted);
        }
        serde_json::from_slice(&body).map_err(|_| ProviderError::Invalid)
    }
}

fn transport_error(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Network
    }
}

async fn read_capped(response: reqwest::Response) -> Result<Vec<u8>, ProviderError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(ProviderError::Invalid);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(transport_error)?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(ProviderError::Invalid);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn failure_kind(error: &ProviderError) -> FailureKind {
    match error {
        ProviderError::Timeout => FailureKind::Timeout,
        ProviderError::RateLimited => FailureKind::RateLimited,
        ProviderError::Auth => FailureKind::Auth,
        ProviderError::QuotaExhausted => FailureKind::Budget,
        ProviderError::Invalid => FailureKind::InvalidResponse,
        _ => FailureKind::Unreachable,
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn endpoint(base: &str, path: &str) -> Result<Url, ProviderError> {
    let base = base.trim_end_matches('/');
    Url::parse(&format!("{base}/{path}")).map_err(|_| ProviderError::Invalid)
}

// ════════════════════════════════════════════════════════════
// Search requests
// ════════════════════════════════════════════════════════════

pub(super) fn search_call(
    kind: ProviderKind,
    base_url: &str,
    request: &SearchRequest,
    today: NaiveDate,
) -> Result<Call, ProviderError> {
    let count = request.max_results.clamp(1, kind.max_results());
    let url = endpoint(base_url, "search")?;
    let query_kind = request.query_kind;
    Ok(match kind {
        ProviderKind::AnySearch => {
            let mut body = json!({"query": request.query, "max_results": count});
            // `tag=code.doc` alone is rejected with 400 ("Missing required
            // params for tag 'code.doc': library", live check 2026-10-30), and
            // the design only allows query_kind to *add* options. Keep the
            // safe `zone` hint and leave the vertical tag off.
            if query_kind == Some(QueryKind::Chinese) {
                body["zone"] = json!("cn");
            }
            Call::Post(url, body)
        }
        ProviderKind::Brave => {
            let mut url = url;
            {
                let mut pairs = url.query_pairs_mut();
                pairs
                    .append_pair("q", &with_site_operators(&request.query, request))
                    .append_pair("count", &count.to_string())
                    .append_pair("extra_snippets", "true");
                match query_kind {
                    Some(QueryKind::News) => {
                        pairs.append_pair("freshness", "pm");
                    }
                    Some(QueryKind::Chinese) => {
                        pairs.append_pair("search_lang", "zh-hans");
                    }
                    _ => {}
                }
            }
            Call::Get(url)
        }
        ProviderKind::DeepSeek => {
            let mut query = with_site_operators(&request.query, request);
            if query_kind == Some(QueryKind::News) {
                let after = today - chrono::Duration::days(NEWS_WINDOW_DAYS);
                query.push_str(&format!(" after:{}", after.format("%Y-%m-%d")));
            }
            Call::Post(url, json!({"query": query, "count": count}))
        }
        ProviderKind::Exa => {
            let mut body = json!({
                "query": request.query,
                "numResults": count,
                "type": "auto",
                // Exa returns no snippet by default (external-search 3.3.1).
                "contents": {"highlights": {"maxCharacters": 300}},
            });
            let allowed = clean_domains(&request.allowed_domains);
            let blocked = clean_domains(&request.blocked_domains);
            if !allowed.is_empty() {
                body["includeDomains"] = json!(allowed);
            }
            if !blocked.is_empty() {
                body["excludeDomains"] = json!(blocked);
            }
            Call::Post(url, body)
        }
        ProviderKind::Firecrawl => Call::Post(
            url,
            json!({"query": with_site_operators(&request.query, request), "limit": count}),
        ),
        ProviderKind::Tavily => {
            let mut body = json!({
                "query": request.query,
                "max_results": count,
                "include_answer": false,
                "include_raw_content": false,
            });
            let allowed = clean_domains(&request.allowed_domains);
            let blocked = clean_domains(&request.blocked_domains);
            if !allowed.is_empty() {
                body["include_domains"] = json!(allowed);
            }
            if !blocked.is_empty() {
                body["exclude_domains"] = json!(blocked);
            }
            Call::Post(url, body)
        }
        ProviderKind::Searxng => {
            let mut url = url;
            {
                let mut pairs = url.query_pairs_mut();
                pairs
                    .append_pair("q", &with_site_operators(&request.query, request))
                    .append_pair("format", "json");
                match query_kind {
                    Some(QueryKind::News) => {
                        pairs.append_pair("time_range", "month");
                    }
                    Some(QueryKind::Chinese) => {
                        pairs.append_pair("language", "zh-CN");
                    }
                    _ => {}
                }
            }
            Call::Get(url)
        }
    })
}

/// Domains for provider-side filters. Only plain host characters survive, so a
/// model-supplied domain cannot smuggle query operators.
fn clean_domains(domains: &[String]) -> Vec<String> {
    domains
        .iter()
        .map(|domain| domain.trim().trim_matches('.').to_ascii_lowercase())
        .filter(|domain| {
            !domain.is_empty()
                && domain
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        })
        .collect()
}

/// `site:` / `-site:` operators for providers without a filter parameter.
/// Results are still domain-filtered locally; this only raises recall.
fn with_site_operators(query: &str, request: &SearchRequest) -> String {
    let allowed = clean_domains(&request.allowed_domains);
    let blocked = clean_domains(&request.blocked_domains);
    let mut result = query.to_string();
    match allowed.as_slice() {
        [] => {}
        [only] => result.push_str(&format!(" site:{only}")),
        many => result.push_str(&format!(
            " ({})",
            many.iter()
                .map(|domain| format!("site:{domain}"))
                .collect::<Vec<_>>()
                .join(" OR ")
        )),
    }
    for domain in blocked {
        result.push_str(&format!(" -site:{domain}"));
    }
    result
}

// ════════════════════════════════════════════════════════════
// Fetch requests (external-search 4.6)
// ════════════════════════════════════════════════════════════

pub(super) fn fetch_call(
    kind: ProviderKind,
    base_url: &str,
    target: &str,
) -> Result<Call, ProviderError> {
    // Live fetch is always requested: Firecrawl and Exa serve cached pages by
    // default (measured: a day old).
    let (path, body) = match kind {
        // AnySearch `/v1/extract` takes a single `url`; `urls[]` is rejected
        // with 400 `invalid_extract_url` (live check 2026-10-30).
        ProviderKind::AnySearch => ("extract", json!({"url": target})),
        ProviderKind::Tavily => ("extract", json!({"urls": [target]})),
        ProviderKind::DeepSeek => ("fetch", json!({"url": target})),
        ProviderKind::Exa => (
            "contents",
            json!({"urls": [target], "text": true, "maxAgeHours": 0}),
        ),
        ProviderKind::Firecrawl => (
            "scrape",
            json!({"url": target, "formats": ["markdown"], "maxAge": 0}),
        ),
        ProviderKind::Brave | ProviderKind::Searxng => return Err(ProviderError::Unsupported),
    };
    Ok(Call::Post(endpoint(base_url, path)?, body))
}
