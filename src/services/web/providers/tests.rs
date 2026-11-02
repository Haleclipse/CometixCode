//! Provider contract tests against a local mock (external-search 8 / 9).
//!
//! Nothing here touches a real provider or a real key.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::client::{Call, fetch_call, search_call};
use super::*;
use crate::services::web::test_support::key_file;

// ════════════════════════════════════════════════════════════
// Mock server
// ════════════════════════════════════════════════════════════

#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

pub(crate) struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, &'static str)>,
    pub body: String,
    pub delay: Duration,
}

impl Reply {
    pub fn ok(body: Value) -> Self {
        Self::status(200, body.to_string())
    }

    pub fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

pub(crate) struct Mock {
    pub url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    pub fn only_request(&self) -> Recorded {
        let requests = self.requests();
        assert_eq!(requests.len(), 1, "expected exactly one request");
        requests.into_iter().next().unwrap()
    }
}

/// Serves `replies` in order, one per connection; the last reply repeats.
pub(crate) async fn mock(replies: Vec<Reply>) -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&requests);
    let replies = Arc::new(replies);
    tokio::spawn(async move {
        let mut served = 0usize;
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            let reply_index = served.min(replies.len() - 1);
            served += 1;
            let replies = Arc::clone(&replies);
            tokio::spawn(async move {
                let Some(recorded) = read_request(&mut stream).await else {
                    return;
                };
                log.lock().unwrap().push(recorded);
                let reply = &replies[reply_index];
                tokio::time::sleep(reply.delay).await;
                let mut head = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (name, value) in &reply.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(reply.body.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Mock { url, requests }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<Recorded> {
    let mut buffer = Vec::new();
    let header_end = loop {
        let mut chunk = [0u8; 4096];
        let count = stream.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..count]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect::<Vec<_>>();
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() - header_end < length {
        let mut chunk = [0u8; 4096];
        let count = stream.read(&mut chunk).await.ok()?;
        if count == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..count]);
    }
    Some(Recorded {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&buffer[header_end..]).to_string(),
    })
}

fn request(query: &str, max_results: usize) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        max_results,
        allowed_domains: Vec::new(),
        blocked_domains: Vec::new(),
        query_kind: None,
    }
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 29).unwrap()
}

fn post_body(call: Call) -> (String, Value) {
    match call {
        Call::Post(url, body) => (url.to_string(), body),
        Call::Get(url) => panic!("expected POST, got GET {url}"),
    }
}

fn get_url(call: Call) -> reqwest::Url {
    match call {
        Call::Get(url) => url,
        Call::Post(url, _) => panic!("expected GET, got POST {url}"),
    }
}

fn query_pairs(url: &reqwest::Url) -> Vec<(String, String)> {
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn param<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

// ════════════════════════════════════════════════════════════
// Error type
// ════════════════════════════════════════════════════════════

#[test]
fn provider_errors_are_named_without_response_bodies() {
    assert_eq!(ProviderError::QuotaExhausted.code(), "quota_exhausted");
    assert_eq!(ProviderError::Http(503).code(), "http_503");
    assert_eq!(ProviderError::Auth.code(), "auth_failed");
    assert_eq!(ProviderError::Invalid.code(), "invalid_response");
}

// ════════════════════════════════════════════════════════════
// Request bodies (pure)
// ════════════════════════════════════════════════════════════

#[test]
fn search_bodies_cap_the_count_per_provider() {
    let wanted = request("rust reqwest", 500);
    let cases = [
        (ProviderKind::AnySearch, "max_results", 10),
        (ProviderKind::DeepSeek, "count", 50),
        (ProviderKind::Exa, "numResults", 100),
        (ProviderKind::Firecrawl, "limit", 100),
        (ProviderKind::Tavily, "max_results", 20),
    ];
    for (kind, field, cap) in cases {
        let (url, body) =
            post_body(search_call(kind, "https://api.test/v1/", &wanted, today()).unwrap());
        assert_eq!(url, "https://api.test/v1/search", "{kind:?}");
        assert_eq!(body[field], json!(cap), "{kind:?}");
    }
    for (kind, cap) in [(ProviderKind::Brave, 20), (ProviderKind::Searxng, 20)] {
        let url = get_url(search_call(kind, "https://api.test", &wanted, today()).unwrap());
        let pairs = query_pairs(&url);
        assert_eq!(param(&pairs, "q"), Some("rust reqwest"), "{kind:?}");
        if kind == ProviderKind::Brave {
            assert_eq!(param(&pairs, "count"), Some("20"));
            assert_eq!(cap, 20);
        }
    }
}

#[test]
fn search_bodies_never_ask_for_full_page_text() {
    let wanted = request("q", 20);
    for kind in [
        ProviderKind::AnySearch,
        ProviderKind::DeepSeek,
        ProviderKind::Exa,
        ProviderKind::Firecrawl,
        ProviderKind::Tavily,
    ] {
        let (_, body) = post_body(search_call(kind, "https://api.test", &wanted, today()).unwrap());
        let text = body.to_string();
        for forbidden in [
            "include_raw_content\":true",
            "scrapeOptions",
            "\"text\":true",
        ] {
            assert!(!text.contains(forbidden), "{kind:?}: {text}");
        }
    }
    let (_, exa) =
        post_body(search_call(ProviderKind::Exa, "https://api.exa.ai", &wanted, today()).unwrap());
    assert_eq!(exa["contents"]["highlights"]["maxCharacters"], 300);
    assert_eq!(exa["type"], "auto");
    let (_, tavily) =
        post_body(search_call(ProviderKind::Tavily, "https://api.test", &wanted, today()).unwrap());
    assert_eq!(tavily["include_answer"], false);
    assert_eq!(tavily["include_raw_content"], false);
}

#[test]
fn brave_asks_for_extra_snippets_and_searxng_for_json() {
    let wanted = request("q", 8);
    let brave = query_pairs(&get_url(
        search_call(
            ProviderKind::Brave,
            "https://api.test/res/v1/web",
            &wanted,
            today(),
        )
        .unwrap(),
    ));
    assert_eq!(param(&brave, "extra_snippets"), Some("true"));
    let searxng = query_pairs(&get_url(
        search_call(
            ProviderKind::Searxng,
            "https://sx.test:8454",
            &wanted,
            today(),
        )
        .unwrap(),
    ));
    assert_eq!(param(&searxng, "format"), Some("json"));
}

#[test]
fn query_kind_adds_parameters_without_removing_any() {
    let mut chinese = request("大模型 上下文压缩", 10);
    chinese.query_kind = Some(QueryKind::Chinese);
    let (_, any) = post_body(
        search_call(ProviderKind::AnySearch, "https://a.test", &chinese, today()).unwrap(),
    );
    assert_eq!(any["zone"], "cn");
    assert_eq!(any["query"], "大模型 上下文压缩");

    let mut docs = request("tokio select", 10);
    docs.query_kind = Some(QueryKind::TechDocs);
    let (_, any) =
        post_body(search_call(ProviderKind::AnySearch, "https://a.test", &docs, today()).unwrap());
    // AnySearch rejects `tag` without its `library` companion (400), so the
    // tech-doc hint must not be forwarded as a tag.
    assert!(any.get("tag").is_none());
    assert!(any.get("zone").is_none());

    let mut news = request("rust release", 10);
    news.query_kind = Some(QueryKind::News);
    let brave = query_pairs(&get_url(
        search_call(ProviderKind::Brave, "https://b.test", &news, today()).unwrap(),
    ));
    assert_eq!(param(&brave, "freshness"), Some("pm"));
    let (_, deepseek) =
        post_body(search_call(ProviderKind::DeepSeek, "https://d.test", &news, today()).unwrap());
    assert_eq!(deepseek["query"], "rust release after:2026-09-28");
    let searxng = query_pairs(&get_url(
        search_call(ProviderKind::Searxng, "https://s.test", &news, today()).unwrap(),
    ));
    assert_eq!(param(&searxng, "time_range"), Some("month"));

    // General queries and an unanswered decision add nothing.
    let mut general = request("rust release", 10);
    general.query_kind = Some(QueryKind::General);
    let plain = request("rust release", 10);
    for wanted in [&general, &plain] {
        let brave = query_pairs(&get_url(
            search_call(ProviderKind::Brave, "https://b.test", wanted, today()).unwrap(),
        ));
        assert_eq!(param(&brave, "freshness"), None);
        let (_, deepseek) = post_body(
            search_call(ProviderKind::DeepSeek, "https://d.test", wanted, today()).unwrap(),
        );
        assert_eq!(deepseek["query"], "rust release");
    }
}

#[test]
fn domain_filters_use_native_fields_or_site_operators() {
    let mut wanted = request("docs", 10);
    wanted.allowed_domains = vec!["Docs.Rs".to_string(), "doc.rust-lang.org".to_string()];
    wanted.blocked_domains = vec!["spam.example".to_string(), "bad domain; drop".to_string()];

    let (_, exa) =
        post_body(search_call(ProviderKind::Exa, "https://e.test", &wanted, today()).unwrap());
    assert_eq!(
        exa["includeDomains"],
        json!(["docs.rs", "doc.rust-lang.org"])
    );
    assert_eq!(exa["excludeDomains"], json!(["spam.example"]));

    let (_, tavily) =
        post_body(search_call(ProviderKind::Tavily, "https://t.test", &wanted, today()).unwrap());
    assert_eq!(
        tavily["include_domains"],
        json!(["docs.rs", "doc.rust-lang.org"])
    );
    assert_eq!(tavily["exclude_domains"], json!(["spam.example"]));

    let (_, deepseek) =
        post_body(search_call(ProviderKind::DeepSeek, "https://d.test", &wanted, today()).unwrap());
    assert_eq!(
        deepseek["query"],
        "docs (site:docs.rs OR site:doc.rust-lang.org) -site:spam.example"
    );

    let mut single = request("docs", 10);
    single.allowed_domains = vec!["docs.rs".to_string()];
    let (_, firecrawl) = post_body(
        search_call(ProviderKind::Firecrawl, "https://f.test", &single, today()).unwrap(),
    );
    assert_eq!(firecrawl["query"], "docs site:docs.rs");
}

#[test]
fn fetch_bodies_request_live_pages() {
    let target = "https://doc.rust-lang.org/std/collections/struct.HashMap.html";
    let cases = [
        (ProviderKind::AnySearch, "extract", json!({"url": target})),
        (ProviderKind::Tavily, "extract", json!({"urls": [target]})),
        (ProviderKind::DeepSeek, "fetch", json!({"url": target})),
        (
            ProviderKind::Exa,
            "contents",
            json!({"urls": [target], "text": true, "maxAgeHours": 0}),
        ),
        (
            ProviderKind::Firecrawl,
            "scrape",
            json!({"url": target, "formats": ["markdown"], "maxAge": 0}),
        ),
    ];
    for (kind, path, expected) in cases {
        let (url, body) = post_body(fetch_call(kind, "https://api.test/base/", target).unwrap());
        assert_eq!(url, format!("https://api.test/base/{path}"), "{kind:?}");
        assert_eq!(body, expected, "{kind:?}");
    }
    for kind in [ProviderKind::Brave, ProviderKind::Searxng] {
        assert_eq!(
            fetch_call(kind, "https://api.test", target),
            Err(ProviderError::Unsupported)
        );
    }
}

// ════════════════════════════════════════════════════════════
// Over the wire
// ════════════════════════════════════════════════════════════

const DEADLINE: Duration = Duration::from_secs(5);

fn hit_envelope(kind: &str) -> Value {
    let item = json!({"title": "T", "url": "https://example.com/a", "snippet": "s",
                      "description": "s", "content": "s", "highlights": ["s"]});
    match kind {
        "brave" => json!({"web": {"results": [item]}}),
        "firecrawl" => json!({"data": {"web": [item]}}),
        _ => json!({"results": [item]}),
    }
}

#[tokio::test]
async fn every_provider_sends_its_auth_header_and_parses_hits() {
    let cases: [(
        &str,
        fn(&str, Option<String>) -> Result<Box<dyn SearchProvider>, ProviderError>,
        &str,
        &str,
    ); 7] = [
        (
            "anysearch",
            |u, k| Ok(Box::new(AnySearchProvider::with_base_url(u, k)?)),
            "authorization",
            "Bearer sekret",
        ),
        (
            "brave",
            |u, k| Ok(Box::new(BraveProvider::with_base_url(u, k)?)),
            "x-subscription-token",
            "sekret",
        ),
        (
            "deepseek",
            |u, k| Ok(Box::new(DeepSeekProvider::with_base_url(u, k)?)),
            "authorization",
            "Bearer sekret",
        ),
        (
            "exa",
            |u, k| Ok(Box::new(ExaProvider::with_base_url(u, k)?)),
            "x-api-key",
            "sekret",
        ),
        (
            "firecrawl",
            |u, k| Ok(Box::new(FirecrawlProvider::with_base_url(u, k)?)),
            "authorization",
            "Bearer sekret",
        ),
        (
            "tavily",
            |u, k| Ok(Box::new(TavilyProvider::with_base_url(u, k)?)),
            "authorization",
            "Bearer sekret",
        ),
        (
            "searxng",
            |u, k| Ok(Box::new(SearxngProvider::with_base_url(u, k)?)),
            "",
            "",
        ),
    ];
    for (name, build, header, expected) in cases {
        let server = mock(vec![Reply::ok(hit_envelope(name))]).await;
        let provider = build(&server.url, Some("sekret".to_string())).unwrap();
        assert_eq!(provider.name(), name);
        let hits = provider.search(request("rust", 5), DEADLINE).await.unwrap();
        assert_eq!(hits.len(), 1, "{name}");
        assert_eq!(hits[0].url, "https://example.com/a", "{name}");

        let seen = server.only_request();
        if header.is_empty() {
            // SearXNG is unauthenticated and must not leak a key it was handed.
            assert!(seen.header("authorization").is_none());
            assert!(seen.header("x-api-key").is_none());
        } else {
            assert_eq!(seen.header(header), Some(expected), "{name}");
        }
        assert!(
            seen.target.starts_with("/search"),
            "{name}: {}",
            seen.target
        );
        assert_eq!(
            seen.method,
            if matches!(name, "brave" | "searxng") {
                "GET"
            } else {
                "POST"
            }
        );
    }
}

#[tokio::test]
async fn a_post_search_sends_the_documented_json_body() {
    let server = mock(vec![Reply::ok(hit_envelope("exa"))]).await;
    let provider = ExaProvider::with_base_url(&server.url, Some("k".to_string())).unwrap();
    provider
        .search(request("tokio", 7), DEADLINE)
        .await
        .unwrap();
    let seen = server.only_request();
    assert_eq!(seen.header("content-type"), Some("application/json"));
    let body = seen.json();
    assert_eq!(body["query"], "tokio");
    assert_eq!(body["numResults"], 7);
    assert_eq!(body["contents"]["highlights"]["maxCharacters"], 300);
}

#[tokio::test]
async fn results_are_trimmed_to_the_requested_count() {
    let items = (0..6)
        .map(|index| json!({"title": index.to_string(), "url": format!("https://example.com/{index}"), "content": "c"}))
        .collect::<Vec<_>>();
    let server = mock(vec![Reply::ok(json!({"results": items}))]).await;
    let provider = TavilyProvider::with_base_url(&server.url, Some("k".to_string())).unwrap();
    let hits = provider.search(request("q", 3), DEADLINE).await.unwrap();
    assert_eq!(hits.len(), 3);
}

#[tokio::test]
async fn fetch_goes_to_the_documented_path_with_a_live_flag() {
    let page = json!({"data": {"markdown": "# Doc\nbody", "metadata": {"statusCode": 200}}});
    let server = mock(vec![Reply::ok(page)]).await;
    let provider = FirecrawlProvider::with_base_url(&server.url, Some("k".to_string())).unwrap();
    let page = provider
        .fetch("https://example.com/doc".to_string(), DEADLINE)
        .await
        .unwrap();
    assert_eq!(page.content, "# Doc\nbody");
    let seen = server.only_request();
    assert_eq!(seen.target, "/scrape");
    assert_eq!(seen.json()["maxAge"], 0);
    assert_eq!(seen.json()["url"], "https://example.com/doc");
}

#[tokio::test]
async fn fetch_403_keeps_the_raw_status_while_fetch_401_is_auth() {
    // Search still maps 403 to Auth (see `http_statuses_map_to_named_errors...`),
    // but the fetch path treats 403 as a page-level failure so one unreadable
    // page cannot drop a provider for an hour (provider-health 3.6).
    let server = mock(vec![Reply::status(403, "{}"), Reply::status(401, "{}")]).await;
    let provider = FirecrawlProvider::with_base_url(&server.url, Some("k".to_string())).unwrap();
    assert_eq!(
        provider
            .fetch("https://example.com/x".to_string(), DEADLINE)
            .await,
        Err(ProviderError::Http(403))
    );
    assert_eq!(
        provider
            .fetch("https://example.com/x".to_string(), DEADLINE)
            .await,
        Err(ProviderError::Auth)
    );
}

#[tokio::test]
async fn a_keyed_provider_without_a_key_makes_no_request() {
    let server = mock(vec![Reply::ok(hit_envelope("exa"))]).await;
    let provider = ExaProvider::with_base_url(&server.url, None).unwrap();
    assert_eq!(
        provider.search(request("secret query", 5), DEADLINE).await,
        Err(ProviderError::Auth)
    );
    assert!(server.requests().is_empty());
    // AnySearch also serves anonymous callers.
    let any = mock(vec![Reply::ok(hit_envelope("anysearch"))]).await;
    let provider = AnySearchProvider::with_base_url(&any.url, None).unwrap();
    assert!(provider.search(request("q", 5), DEADLINE).await.is_ok());
    assert!(any.only_request().header("authorization").is_none());
}

// ════════════════════════════════════════════════════════════
// Error bodies over the wire
// ════════════════════════════════════════════════════════════

async fn search_error(
    build: impl FnOnce(&str) -> Box<dyn SearchProvider>,
    reply: Reply,
) -> ProviderError {
    let server = mock(vec![reply]).await;
    build(&server.url)
        .search(request("q", 5), DEADLINE)
        .await
        .unwrap_err()
}

#[tokio::test]
async fn http_statuses_map_to_named_errors_for_every_provider() {
    type Build = fn(&str) -> Box<dyn SearchProvider>;
    let builders: [(&str, Build); 6] = [
        ("anysearch", |u| {
            Box::new(AnySearchProvider::with_base_url(u, Some("k".into())).unwrap())
        }),
        ("brave", |u| {
            Box::new(BraveProvider::with_base_url(u, Some("k".into())).unwrap())
        }),
        ("deepseek", |u| {
            Box::new(DeepSeekProvider::with_base_url(u, Some("k".into())).unwrap())
        }),
        ("firecrawl", |u| {
            Box::new(FirecrawlProvider::with_base_url(u, Some("k".into())).unwrap())
        }),
        ("tavily", |u| {
            Box::new(TavilyProvider::with_base_url(u, Some("k".into())).unwrap())
        }),
        ("searxng", |u| {
            Box::new(SearxngProvider::with_base_url(u, None).unwrap())
        }),
    ];
    for (name, build) in builders {
        assert_eq!(
            search_error(build, Reply::status(401, "{}")).await,
            ProviderError::Auth,
            "{name} 401"
        );
        assert_eq!(
            search_error(build, Reply::status(403, "{}")).await,
            ProviderError::Auth,
            "{name} 403"
        );
        assert_eq!(
            search_error(build, Reply::status(402, "{}")).await,
            ProviderError::QuotaExhausted,
            "{name} 402"
        );
        assert_eq!(
            search_error(build, Reply::status(429, "{}")).await,
            ProviderError::RateLimited,
            "{name} 429"
        );
        assert_eq!(
            search_error(build, Reply::status(500, "boom")).await,
            ProviderError::Http(500),
            "{name} 500"
        );
        assert_eq!(
            search_error(build, Reply::status(200, "<html>not json</html>")).await,
            ProviderError::Invalid,
            "{name} html"
        );
    }
}

#[tokio::test]
async fn exa_402_is_quota_only_for_the_documented_credit_codes() {
    let build = |u: &str| -> Box<dyn SearchProvider> {
        Box::new(ExaProvider::with_base_url(u, Some("k".into())).unwrap())
    };
    assert_eq!(
        search_error(build, Reply::status(402, r#"{"tag":"NO_MORE_CREDITS"}"#)).await,
        ProviderError::QuotaExhausted
    );
    assert_eq!(
        search_error(build, Reply::status(402, r#"{"error":"payment required"}"#)).await,
        ProviderError::Http(402)
    );
    assert_eq!(
        search_error(build, Reply::status(429, "{}").header("retry-after", "2")).await,
        ProviderError::RateLimited
    );
}

#[tokio::test]
async fn tavily_432_is_quota_exhausted() {
    let build = |u: &str| -> Box<dyn SearchProvider> {
        Box::new(TavilyProvider::with_base_url(u, Some("k".into())).unwrap())
    };
    assert_eq!(
        search_error(
            build,
            Reply::status(
                432,
                r#"{"detail":{"error":"This request exceeds your plan's set usage limit."}}"#
            )
        )
        .await,
        ProviderError::QuotaExhausted
    );
}

#[tokio::test]
async fn brave_monthly_window_zero_is_quota_even_on_a_successful_response() {
    let build = |u: &str| -> Box<dyn SearchProvider> {
        Box::new(BraveProvider::with_base_url(u, Some("k".into())).unwrap())
    };
    let spent = Reply::ok(hit_envelope("brave")).header("x-ratelimit-remaining", "1, 0");
    assert_eq!(
        search_error(build, spent).await,
        ProviderError::QuotaExhausted
    );
    // The per-second window hitting zero on a 429 is a plain rate limit.
    let throttled = Reply::status(429, "{}").header("x-ratelimit-remaining", "0, 1400");
    assert_eq!(
        search_error(build, throttled).await,
        ProviderError::RateLimited
    );
    // A healthy window returns hits.
    let server = mock(vec![
        Reply::ok(hit_envelope("brave")).header("x-ratelimit-remaining", "1, 1400"),
    ])
    .await;
    let provider = BraveProvider::with_base_url(&server.url, Some("k".into())).unwrap();
    assert_eq!(
        provider
            .search(request("q", 5), DEADLINE)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_slow_provider_times_out_by_name() {
    let server = mock(vec![
        Reply::ok(hit_envelope("exa")).delayed(Duration::from_millis(600)),
    ])
    .await;
    let provider = ExaProvider::with_base_url(&server.url, Some("k".into())).unwrap();
    assert_eq!(
        provider
            .search(request("q", 5), Duration::from_millis(100))
            .await,
        Err(ProviderError::Timeout)
    );
}

#[tokio::test]
async fn a_closed_port_is_a_network_error() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let provider =
        ExaProvider::with_base_url(format!("http://127.0.0.1:{port}"), Some("k".into())).unwrap();
    assert_eq!(
        provider.search(request("q", 5), DEADLINE).await,
        Err(ProviderError::Network)
    );
}

#[tokio::test]
async fn error_bodies_never_surface_in_the_error_value() {
    let secret = "sk-live-0123456789abcdef"; // gitleaks:allow -- synthetic provider error fixture
    let body =
        format!(r#"{{"error":"bad key {secret}","account":"new-account-9","key":"fresh-key-1"}}"#);
    for status in [400, 401, 402, 429, 500] {
        let server = mock(vec![Reply::status(status, body.clone())]).await;
        let provider =
            AnySearchProvider::with_base_url(&server.url, Some(secret.to_string())).unwrap();
        let error = provider
            .search(request("q", 5), DEADLINE)
            .await
            .unwrap_err();
        let shown = format!("{error} {error:?}");
        for leaked in [secret, "new-account-9", "fresh-key-1", "bad key"] {
            assert!(
                !shown.contains(leaked),
                "status {status} leaked {leaked}: {shown}"
            );
        }
    }
}

// ════════════════════════════════════════════════════════════
// SearXNG availability
// ════════════════════════════════════════════════════════════

#[tokio::test]
async fn a_failing_searxng_goes_down_and_stops_sending_requests() {
    let server = mock(vec![Reply::status(500, "boom")]).await;
    let provider = SearxngProvider::with_base_url(&server.url, None).unwrap();
    assert_eq!(
        provider.search(request("q", 5), DEADLINE).await,
        Err(ProviderError::Http(500))
    );
    assert_eq!(server.requests().len(), 1);
    // Down: no socket is opened for the next search.
    assert_eq!(
        provider.search(request("q", 5), DEADLINE).await,
        Err(ProviderError::Unavailable)
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn other_providers_do_not_use_the_searxng_health_machine() {
    let server = mock(vec![
        Reply::status(500, "boom"),
        Reply::ok(hit_envelope("exa")),
    ])
    .await;
    let provider = ExaProvider::with_base_url(&server.url, Some("k".into())).unwrap();
    assert!(provider.search(request("q", 5), DEADLINE).await.is_err());
    assert!(provider.search(request("q", 5), DEADLINE).await.is_ok());
    assert_eq!(server.requests().len(), 2);
}

// ════════════════════════════════════════════════════════════
// Construction from settings
// ════════════════════════════════════════════════════════════

fn settings(json: Value) -> crate::utils::settings::nyan::WebSettings {
    serde_json::from_value(json).unwrap()
}

#[test]
fn disabled_providers_are_skipped_and_order_is_fixed() {
    let key = key_file();
    let web = settings(json!({"providers": {
        "tavily": {"enabled": true, "apiKeyFile": key},
        "brave": {"enabled": false, "apiKeyFile": key},
        "anysearch": {"enabled": true, "apiKeyFile": key},
        "searxng": {"url": "http://127.0.0.1:1"},
        "deepseek": {"enabled": false},
        "exa": {"enabled": false},
        "firecrawl": {"enabled": false},
    }}));
    let (providers, failures) = configured_providers(&web);
    let names = providers
        .iter()
        .map(|provider| provider.as_search().name())
        .collect::<Vec<_>>();
    assert_eq!(names, ["anysearch", "tavily", "searxng"]);
    assert!(failures.is_empty());
}

#[test]
fn an_unreadable_key_file_is_reported_not_dropped() {
    let directory = tempfile_dir();
    let key = directory.join("empty-key");
    std::fs::write(&key, "  \n").unwrap();
    let web = settings(json!({"providers": {
        "exa": {"enabled": true, "apiKeyFile": key.to_string_lossy()},
        "anysearch": {"enabled": true, "apiKeyFile": key_file()},
    }}));
    let (providers, failures) = configured_providers(&web);
    let names = providers
        .iter()
        .map(|provider| provider.as_search().name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "anysearch",
            "brave",
            "deepseek",
            "firecrawl",
            "tavily",
            "searxng"
        ]
    );
    assert_eq!(
        failures,
        vec![ProviderBuildFailure {
            provider: "exa".to_string(),
            error: ProviderError::Auth,
        }]
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_missing_key_file_builds_a_keyless_provider() {
    let web = settings(json!({"providers": {
        "exa": {"enabled": true, "apiKeyFile": "/nonexistent/dir/key"},
    }}));
    let (providers, failures) = configured_providers(&web);
    let names = providers
        .iter()
        .map(|provider| provider.as_search().name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "anysearch",
            "brave",
            "deepseek",
            "exa",
            "firecrawl",
            "tavily",
            "searxng"
        ]
    );
    assert!(failures.is_empty());
}

#[test]
fn an_absent_provider_map_configures_nothing() {
    let (providers, failures) = configured_providers(&settings(json!({})));
    assert!(providers.is_empty());
    assert!(failures.is_empty());
}

#[test]
fn configuring_only_searxng_still_loads_every_builtin() {
    let web = settings(json!({"providers": {
        "searxng": {"url": "http://127.0.0.1:9"},
    }}));
    let (providers, failures) = configured_providers(&web);
    let names = providers
        .iter()
        .map(|provider| provider.as_search().name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "anysearch",
            "brave",
            "deepseek",
            "exa",
            "firecrawl",
            "tavily",
            "searxng"
        ]
    );
    assert!(failures.is_empty());
    assert_eq!(
        web.provider("searxng").url.as_deref(),
        Some("http://127.0.0.1:9"),
        "the user's searxng entry overrides the builtin URL"
    );
    assert!(web.provider("brave").enabled);
    assert_eq!(
        web.provider("brave").api_key_file.as_deref(),
        Some("~/.nyan/keys/brave")
    );
}

#[test]
fn capabilities_match_the_provider_table() {
    let key = key_file();
    let keyed = json!({"apiKeyFile": key});
    let web = settings(json!({"providers": {
        "anysearch": keyed, "brave": keyed, "deepseek": keyed, "exa": keyed,
        "firecrawl": keyed, "tavily": keyed, "searxng": {},
    }}));
    let (providers, _) = configured_providers(&web);
    let capabilities = providers
        .iter()
        .map(|provider| {
            (
                provider.as_search().name(),
                provider.as_search().max_results(),
                provider.as_fetch().is_ok(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        capabilities,
        [
            ("anysearch", 10, true),
            ("brave", 20, false),
            ("deepseek", 50, true),
            ("exa", 100, true),
            ("firecrawl", 100, true),
            ("tavily", 20, true),
            ("searxng", 20, false),
        ]
    );
}

fn tempfile_dir() -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("nyan-providers-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&path).unwrap();
    path
}
