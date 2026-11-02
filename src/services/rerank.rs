//! Shared rerank client (retrieval 4.9, slice G0).
//!
//! @nyan: one client for both code search and web-search precision rerank.
//! The service address is configured once (`nyan.retrieval.rerank.url`); the
//! two switches are independent (`nyan.retrieval.rerank.enabled`, default off;
//! `nyan.web.search.rerank.enabled`, default on).
//!
//! **The official Qwen3-Reranker instruction template is mandatory.** Measured
//! 2026-10-27 on the same related / unrelated / semi-related docs: bare
//! `text_1=<query>, text_2=docs` scored 0.878 / 0.145 / 0.836 (a semi-related
//! doc nearly ties a relevant one), while the template scored 0.999 / 0.000 /
//! 0.010. The deployed `probe.sh` and `bench-rerank.py` use the bare form,
//! which is the likely reason legacy recorded "rerank is a net negative".
//!
//! The template string enters the retrieval generation (retrieval 4.8): a
//! template change is a behavior change and must invalidate old indexes /
//! calibrations.
//!
//! The service machine (§3.2): connect 1.5 s, request 2 s, one shared Down
//! window with retrieval and web search. While Down no request is made and the
//! caller keeps its fusion order. A score outside [0, 1] or non-finite voids
//! the **whole batch** — partial acceptance would silently produce a
//! different ordering than the evaluator measured.
//!
//! Each document is truncated to [`DEFAULT_MAX_DOCUMENT_CHARS`] (overridable
//! with `nyan.retrieval.rerank.maxDocumentChars`) before it is wrapped. The
//! deployed vLLM rejects a prompt past `max_model_len` 4096 with HTTP 400, and
//! one `Cargo.lock`-sized snippet is enough to do that and trip the health
//! machine. Truncation applies only to the bytes posted to `/score`.

use std::time::Duration;

use serde_json::{Value, json};

use crate::services::service_health::{self, CallPermit, FailureKind, ServiceKind};
use crate::utils::settings::nyan::RerankSettings;

/// The official Qwen3-Reranker instruction template, byte for byte
/// (retrieval 4.9). `\n` inside is part of the protocol, not formatting.
pub const TEMPLATE_VERSION: &str = "qwen3-reranker-instruct-v1";

pub const CODE_INSTRUCT: &str =
    "Given a code search query, retrieve relevant code snippets that answer the query";
pub const WEB_INSTRUCT: &str =
    "Given a web search query, retrieve relevant passages that answer the query";

/// Characters kept from each document before the Qwen3 template is applied.
///
/// `qwen3-reranker-0.6b` is served with `max_model_len` 4096 and returns HTTP
/// 400 for a longer prompt. Random text tokenizes at about one token per
/// character, and the instruction template plus the query need on the order of
/// a thousand tokens, so 3000 characters stays inside the limit with margin.
pub const DEFAULT_MAX_DOCUMENT_CHARS: usize = 3_000;

/// The rendered `text_1` for a query and instruction.
pub fn instruction_text(instruct: &str, query: &str) -> String {
    format!(
        "<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n<Instruct>: {instruct}\n<Query>: {query}\n"
    )
}

/// The rendered `text_2` for a document.
pub fn document_text(document: &str) -> String {
    format!("<Document>: {document}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n")
}

/// Which instruction a caller uses. Recorded in the receipt so evaluators can
/// tell code ranking from web ranking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RerankKind {
    Code,
    Web,
}

impl RerankKind {
    pub fn instruct(self) -> &'static str {
        match self {
            RerankKind::Code => CODE_INSTRUCT,
            RerankKind::Web => WEB_INSTRUCT,
        }
    }
}

/// One rerank request ready for the wire.
#[derive(Clone, Debug)]
pub struct RerankRequest {
    pub kind: RerankKind,
    pub query: String,
    pub documents: Vec<String>,
}

/// Per-document score, index-aligned with the request.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankOutcome {
    pub scores: Vec<f64>,
    pub latency: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RerankFailure {
    /// The shared health machine has the service Down.
    Unavailable,
    Timeout,
    /// Non-2xx other than the classified ones.
    Http,
    /// Count mismatch, non-numeric, non-finite, or outside [0, 1].
    InvalidResponse,
}

/// Rerank one batch. `Ok(None)` means "not configured / disabled at the service
/// level, use the fusion order silently" — the caller must not report it as a
/// degradation (retrieval 4.10: rerank failures are a ranking-quality issue,
/// not a result-truth issue).
pub struct RerankClient {
    url: String,
    model: String,
    client: reqwest::Client,
    health: std::sync::Arc<service_health::ServiceHealth>,
    connect_timeout: Duration,
    request_timeout: Duration,
    max_document_chars: usize,
}

impl std::fmt::Debug for RerankClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RerankClient")
            .field("url", &self.url)
            .field("model", &self.model)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

impl RerankClient {
    /// Build from `nyan.retrieval.rerank`. No network, no key (loopback and
    /// tailnet serve are unauthenticated, retrieval 3.1). Clients never go
    /// through the proxy: the address is a local or tailnet one.
    pub fn from_settings(
        settings: &RerankSettings,
        request_timeout_ms: u64,
    ) -> anyhow::Result<Self> {
        let health = service_health::shared(ServiceKind::Rerank);
        // `client_builder` is the one allowed reqwest constructor (clippy.toml);
        // `.no_proxy()` overrides any proxy it would otherwise pick up.
        let client = crate::utils::proxy::client_builder()
            .no_proxy()
            .connect_timeout(health.connect_timeout())
            .timeout(Duration::from_millis(request_timeout_ms))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .map_err(|error| anyhow::anyhow!("cannot build the rerank client: {error}"))?;
        let mut client = Self {
            url: settings.url().to_string(),
            model: settings.model().to_string(),
            client,
            health,
            connect_timeout: service_health::ServiceKind::Rerank.default_timeouts().0,
            request_timeout: Duration::from_millis(request_timeout_ms),
            max_document_chars: settings.max_document_chars() as usize,
        };
        client.install_probe();
        Ok(client)
    }

    /// Test/mock seam.
    pub fn with_client(
        url: impl Into<String>,
        model: impl Into<String>,
        client: reqwest::Client,
        health: std::sync::Arc<service_health::ServiceHealth>,
    ) -> Self {
        let connect_timeout = health.connect_timeout();
        let request_timeout = health.request_timeout();
        let mut client = Self {
            url: url.into(),
            model: model.into(),
            client,
            health,
            connect_timeout,
            request_timeout,
            max_document_chars: DEFAULT_MAX_DOCUMENT_CHARS,
        };
        client.install_probe();
        client
    }

    /// Override the per-document cap. `usize::MAX` disables truncation.
    pub fn with_max_document_chars(mut self, max_document_chars: usize) -> Self {
        self.max_document_chars = max_document_chars;
        self
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Probe: `GET /v1/models`, checking the served model id
    /// (retrieval 3.2). vLLM exposes it on the same port as `/score`.
    fn install_probe(&mut self) {
        let base = self.url.trim_end_matches('/').to_string();
        let model = self.model.clone();
        let client = self.client.clone();
        let request_timeout = self.request_timeout;
        self.health.set_probe(Some(std::sync::Arc::new(move || {
            let base = base.clone();
            let model = model.clone();
            let client = client.clone();
            Box::pin(async move {
                let url = format!("{base}/v1/models");
                let response = tokio::time::timeout(request_timeout, client.get(&url).send())
                    .await
                    .map_err(|_| ())?
                    .map_err(|_| ())?;
                if !response.status().is_success() {
                    return Err(());
                }
                let body: Value = response.json().await.map_err(|_| ())?;
                let serves_model =
                    body.get("data")
                        .and_then(Value::as_array)
                        .is_some_and(|models| {
                            models.iter().any(|entry| {
                                entry
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .is_some_and(|id| id == model)
                            })
                        });
                if serves_model { Ok(()) } else { Err(()) }
            })
        })));
    }

    /// Score `documents` against `query`. `None` means the service may not be
    /// called right now (Down); the caller keeps its existing order.
    pub async fn score(
        &self,
        request: &RerankRequest,
        deadline: Duration,
    ) -> Result<RerankOutcome, RerankFailure> {
        if request.documents.is_empty() {
            return Ok(RerankOutcome {
                scores: Vec::new(),
                latency: Duration::ZERO,
            });
        }
        match self.health.call_permit() {
            CallPermit::Skipped => return Err(RerankFailure::Unavailable),
            CallPermit::Probe => {
                self.health.spawn_probe();
                return Err(RerankFailure::Unavailable);
            }
            CallPermit::Allowed => {}
        }

        let text_1 = instruction_text(request.kind.instruct(), &request.query);
        // Copy and cap. `request.documents` is what the caller will return to
        // the model; only the rerank prompt is shortened.
        let text_2: Vec<String> = request
            .documents
            .iter()
            .map(|document| document_text(truncate_chars(document, self.max_document_chars)))
            .collect();
        let body = json!({
            "model": self.model,
            "text_1": text_1,
            "text_2": text_2,
        });
        let url = format!("{}/score", self.url.trim_end_matches('/'));

        let started = std::time::Instant::now();
        let exchange = async {
            let response = self
                .client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|error| {
                    if error.is_timeout() {
                        (RerankFailure::Timeout, FailureKind::Timeout)
                    } else {
                        (RerankFailure::Http, FailureKind::Unreachable)
                    }
                })?;
            let status = response.status();
            let body: Value = response
                .json()
                .await
                .map_err(|_| (RerankFailure::InvalidResponse, FailureKind::InvalidResponse))?;
            Ok::<_, (RerankFailure, FailureKind)>((status, body))
        };

        // The caller's deadline is a wall-clock bound; the client's own timeout
        // is the outer guard for callers that pass a longer one.
        let timeout = deadline.min(self.request_timeout).max(self.connect_timeout);
        let (status, body) = match tokio::time::timeout(timeout, exchange).await {
            Ok(Ok(parts)) => parts,
            Ok(Err((failure, kind))) => {
                self.health.record_failure(kind);
                return Err(failure);
            }
            Err(_) => {
                self.health.record_failure(FailureKind::Timeout);
                return Err(RerankFailure::Timeout);
            }
        };
        if !status.is_success() {
            let kind = if status.as_u16() == 429 {
                FailureKind::RateLimited
            } else {
                FailureKind::Unreachable
            };
            self.health.record_failure(kind);
            return Err(RerankFailure::Http);
        }

        // vLLM returns `{data: [{score}]}`; some builds return `{results: …}`.
        let entries = body
            .get("data")
            .or_else(|| body.get("results"))
            .and_then(Value::as_array);
        let Some(entries) = entries else {
            self.health.record_failure(FailureKind::InvalidResponse);
            return Err(RerankFailure::InvalidResponse);
        };
        if entries.len() != request.documents.len() {
            self.health.record_failure(FailureKind::InvalidResponse);
            return Err(RerankFailure::InvalidResponse);
        }
        let mut scores = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(score) = entry.get("score").and_then(Value::as_f64) else {
                self.health.record_failure(FailureKind::InvalidResponse);
                return Err(RerankFailure::InvalidResponse);
            };
            // The whole batch is void if any score is out of range: a partially
            // plausible batch would silently reorder differently from the
            // evaluator's measurement (retrieval 4.9).
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                self.health.record_failure(FailureKind::InvalidResponse);
                return Err(RerankFailure::InvalidResponse);
            }
            scores.push(score);
        }

        self.health.record_success();
        Ok(RerankOutcome {
            scores,
            latency: started.elapsed(),
        })
    }
}

/// Shorten `text` to at most `max_chars` Unicode scalars, never mid-character.
fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// Stable order for a score-descending sort with a caller-supplied tie-break.
/// Kept here so code retrieval and web search cannot drift apart on ties.
pub fn order_by_score(
    scores: &[f64],
    tie_break: impl Fn(usize, usize) -> std::cmp::Ordering,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|&a, &b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| tie_break(a, b))
            .then_with(|| a.cmp(&b))
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::service_health::ServiceHealth;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn health() -> std::sync::Arc<ServiceHealth> {
        std::sync::Arc::new(ServiceHealth::with_timeouts(
            ServiceKind::Rerank,
            Duration::from_millis(500),
            Duration::from_millis(2_000),
        ))
    }

    fn client(url: &str, health: std::sync::Arc<ServiceHealth>) -> RerankClient {
        crate::utils::tls_provider::install_crypto_provider();
        RerankClient::with_client(
            url,
            "qwen3-reranker-0.6b",
            reqwest::Client::builder().no_proxy().build().unwrap(),
            health,
        )
    }

    fn request() -> RerankRequest {
        RerankRequest {
            kind: RerankKind::Code,
            query: "where is auth handled".to_string(),
            documents: vec![
                "fn auth() { /* middleware */ }".to_string(),
                "fn parse_json() {}".to_string(),
            ],
        }
    }

    struct Mock {
        url: String,
        handle: tokio::task::JoinHandle<String>,
    }

    async fn mock(body: &'static str) -> Mock {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let header_end = loop {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0);
                buffer.extend_from_slice(&chunk[..count]);
                if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())?
                })
                .unwrap_or(0);
            while buffer.len() - header_end < length {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0);
                buffer.extend_from_slice(&chunk[..count]);
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buffer).to_string()
        });
        Mock {
            url: format!("http://{address}"),
            handle,
        }
    }

    /// Like [`mock`], but the response depends on the request the client sent.
    async fn mock_inspect<F>(decide: F) -> Mock
    where
        F: Fn(&str) -> (u16, String) + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let decide = std::sync::Arc::new(decide);
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let header_end = loop {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0);
                buffer.extend_from_slice(&chunk[..count]);
                if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())?
                })
                .unwrap_or(0);
            while buffer.len() - header_end < length {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0);
                buffer.extend_from_slice(&chunk[..count]);
            }
            let raw = String::from_utf8_lossy(&buffer).to_string();
            let (status, body) = decide(&raw);
            let reason = if status == 200 { "OK" } else { "Bad Request" };
            let reply = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
            raw
        });
        Mock {
            url: format!("http://{address}"),
            handle,
        }
    }

    #[test]
    fn the_template_is_the_official_qwen3_instruction_pair() {
        let text_1 = instruction_text(CODE_INSTRUCT, "q");
        assert!(text_1.starts_with(
            "<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n"
        ));
        assert!(text_1.contains("<Instruct>: Given a code search query, retrieve relevant code snippets that answer the query\n"));
        assert!(text_1.ends_with("<Query>: q\n"));
        assert_eq!(
            document_text("doc"),
            "<Document>: doc<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        );
        assert!(WEB_INSTRUCT.starts_with("Given a web search query"));
        assert_ne!(CODE_INSTRUCT, WEB_INSTRUCT);
        assert_eq!(TEMPLATE_VERSION, "qwen3-reranker-instruct-v1");
    }

    #[test]
    fn the_wire_request_uses_the_template_and_parses_scores() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mock = mock(r#"{"data":[{"score":0.999},{"score":0.010}]}"#).await;
                let client = client(&mock.url, health());
                let outcome = client
                    .score(&request(), Duration::from_millis(2_000))
                    .await
                    .expect("scores");
                assert_eq!(outcome.scores, vec![0.999, 0.010]);

                let raw = mock.handle.await.unwrap();
                assert!(raw.starts_with("POST /score "), "{raw}");
                let body: Value =
                    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                assert_eq!(body["model"], "qwen3-reranker-0.6b");
                let text_1 = body["text_1"].as_str().unwrap();
                assert!(text_1.contains("<Instruct>: Given a code search query"));
                assert!(text_1.contains("<Query>: where is auth handled"));
                let text_2 = body["text_2"].as_array().unwrap();
                assert_eq!(text_2.len(), 2);
                assert!(
                    text_2[0]
                        .as_str()
                        .unwrap()
                        .starts_with("<Document>: fn auth()")
                );
                assert!(text_2[0].as_str().unwrap().contains("<|im_end|>"));
            });
    }

    #[test]
    fn the_web_instruction_is_used_for_web_rerank() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mock = mock(r#"{"data":[{"score":0.4}]}"#).await;
                let client = client(&mock.url, health());
                let request = RerankRequest {
                    kind: RerankKind::Web,
                    query: "durable tasks".to_string(),
                    documents: vec!["a page".to_string()],
                };
                client
                    .score(&request, Duration::from_millis(2_000))
                    .await
                    .unwrap();
                let raw = mock.handle.await.unwrap();
                let body: Value =
                    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                assert!(
                    body["text_1"]
                        .as_str()
                        .unwrap()
                        .contains("Given a web search query")
                );
            });
    }

    #[test]
    fn an_out_of_range_or_non_finite_score_voids_the_whole_batch() {
        for body in [
            r#"{"data":[{"score":0.5},{"score":1.2}]}"#,
            r#"{"data":[{"score":0.5},{"score":-0.1}]}"#,
            r#"{"data":[{"score":0.5},{"score":"high"}]}"#,
        ] {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    // Leak-free: one connection per mock; the body is a
                    // &'static str from the array.
                    let mock = mock(body).await;
                    let client = client(&mock.url, health());
                    let outcome = client.score(&request(), Duration::from_millis(2_000)).await;
                    assert_eq!(outcome, Err(RerankFailure::InvalidResponse), "{body}");
                });
        }
    }

    #[test]
    fn a_count_mismatch_is_invalid() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mock = mock(r#"{"data":[{"score":0.5}]}"#).await;
                let client = client(&mock.url, health());
                assert_eq!(
                    client.score(&request(), Duration::from_millis(2_000)).await,
                    Err(RerankFailure::InvalidResponse)
                );
            });
    }

    #[test]
    fn a_down_service_returns_unavailable_without_a_request() {
        let health = health();
        health.record_failure(FailureKind::Unreachable);
        let client = client("http://127.0.0.1:1", health);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                assert_eq!(
                    client.score(&request(), Duration::from_millis(2_000)).await,
                    Err(RerankFailure::Unavailable)
                );
            });
    }

    #[test]
    fn an_empty_document_list_scores_without_a_request() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let client = client("http://127.0.0.1:1", health());
                let request = RerankRequest {
                    kind: RerankKind::Web,
                    query: "q".to_string(),
                    documents: Vec::new(),
                };
                let outcome = client
                    .score(&request, Duration::from_millis(2_000))
                    .await
                    .unwrap();
                assert!(outcome.scores.is_empty());
            });
    }

    #[test]
    fn clients_are_built_without_a_proxy() {
        // The address is loopback/tailnet; going through the local proxy would
        // be both wrong and slow (retrieval 3.1).
        let client = RerankClient::from_settings(
            &crate::utils::settings::nyan::RerankSettings::default(),
            2_000,
        )
        .unwrap();
        assert_eq!(client.url(), "http://10.144.0.11:8091");
        assert_eq!(client.model(), "qwen3-reranker-0.6b");
        assert_eq!(client.connect_timeout, Duration::from_millis(1_500));
        assert_eq!(client.request_timeout, Duration::from_millis(2_000));
        assert_eq!(client.max_document_chars, DEFAULT_MAX_DOCUMENT_CHARS);
        assert_eq!(
            crate::utils::settings::nyan::RerankSettings::default().max_document_chars() as usize,
            DEFAULT_MAX_DOCUMENT_CHARS
        );
    }

    #[test]
    fn truncation_stops_on_a_char_boundary() {
        let text = "é".repeat(3);
        assert_eq!(truncate_chars(&text, 2), "éé");
        assert_eq!(truncate_chars(&text, 10), text);
        assert_eq!(truncate_chars("abc", 0), "");
    }

    #[test]
    fn an_overlong_snippet_is_truncated_so_the_reranker_does_not_400() {
        // vLLM returns 400 when one prompt exceeds max_model_len (4096). A
        // Cargo.lock-sized snippet does that today. The mock answers 400 when
        // the rendered document is still long, and 200 once the client has
        // capped it. The caller's own string is not modified.
        const GUARD: usize = 200;
        fn guard(raw: &str) -> (u16, String) {
            let body: Value = serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap_or(""))
                .unwrap_or(Value::Null);
            let too_long = body["text_2"].as_array().is_some_and(|docs| {
                docs.iter()
                    .any(|doc| doc.as_str().is_some_and(|text| text.len() > GUARD))
            });
            if too_long {
                (
                    400,
                    r#"{"error":{"message":"maximum context length is 4096 tokens","code":400}}"#
                        .to_string(),
                )
            } else {
                (200, r#"{"data":[{"score":0.5}]}"#.to_string())
            }
        }

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let oversized = "x".repeat(50_000);
                let request = RerankRequest {
                    kind: RerankKind::Code,
                    query: "INDEX_SCHEMA_VERSION".to_string(),
                    documents: vec![oversized],
                };

                let overflowing = mock_inspect(guard).await;
                let uncapped = client(&overflowing.url, health()).with_max_document_chars(50_000);
                assert_eq!(
                    uncapped.score(&request, Duration::from_secs(2)).await,
                    Err(RerankFailure::Http),
                    "an uncapped Cargo.lock-sized snippet is the 400"
                );

                let capped_mock = mock_inspect(guard).await;
                let capped = client(&capped_mock.url, health()).with_max_document_chars(32);
                let outcome = capped
                    .score(&request, Duration::from_secs(2))
                    .await
                    .expect("the truncated document is accepted");
                assert_eq!(outcome.scores, vec![0.5]);
                let raw = capped_mock.handle.await.unwrap();
                let body: Value =
                    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                let sent = body["text_2"][0].as_str().unwrap();
                assert!(sent.contains(&"x".repeat(32)), "{sent}");
                assert!(!sent.contains(&"x".repeat(33)), "{sent}");
                assert_eq!(request.documents[0].len(), 50_000);
            });
    }

    #[test]
    fn a_cargo_lock_snippet_is_capped_at_the_default_before_it_reaches_score() {
        // Fixture for the eval case: the real Cargo.lock (> 40k chars) sent
        // with the default cap. The mock stands in for vLLM and answers 400
        // whenever a rendered document is far larger than the default cap
        // plus the instruction template.
        const LIMIT: usize = DEFAULT_MAX_DOCUMENT_CHARS + 1_000;
        fn guard(raw: &str) -> (u16, String) {
            let body: Value = serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap_or(""))
                .unwrap_or(Value::Null);
            let too_long = body["text_2"].as_array().is_some_and(|docs| {
                docs.iter().any(|doc| {
                    doc.as_str()
                        .is_some_and(|text| text.chars().count() > LIMIT)
                })
            });
            if too_long {
                (
                    400,
                    r#"{"error":{"message":"maximum context length is 4096 tokens","code":400}}"#
                        .to_string(),
                )
            } else {
                (200, r#"{"data":[{"score":0.25}]}"#.to_string())
            }
        }

        let lockfile = include_str!("../../Cargo.lock");
        assert!(
            lockfile.chars().count() > 40_000,
            "Cargo.lock is the oversized snippet from the eval"
        );
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let request = RerankRequest {
                    kind: RerankKind::Code,
                    query: "INDEX_SCHEMA_VERSION".to_string(),
                    documents: vec![lockfile.to_string()],
                };

                let bare_mock = mock_inspect(guard).await;
                let bare = client(&bare_mock.url, health())
                    .with_max_document_chars(lockfile.chars().count());
                assert_eq!(
                    bare.score(&request, Duration::from_secs(2)).await,
                    Err(RerankFailure::Http),
                    "an untruncated Cargo.lock is the 400"
                );

                let capped_mock = mock_inspect(guard).await;
                let capped = client(&capped_mock.url, health());
                let outcome = capped
                    .score(&request, Duration::from_secs(2))
                    .await
                    .expect("the default cap keeps the prompt inside max_model_len");
                assert_eq!(outcome.scores, vec![0.25]);
                let raw = capped_mock.handle.await.unwrap();
                let body: Value =
                    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                let sent = body["text_2"][0].as_str().unwrap();
                let prefix = truncate_chars(lockfile, DEFAULT_MAX_DOCUMENT_CHARS);
                assert_eq!(prefix.chars().count(), DEFAULT_MAX_DOCUMENT_CHARS);
                assert!(sent.contains(prefix));
                assert!(!sent.contains(truncate_chars(lockfile, DEFAULT_MAX_DOCUMENT_CHARS + 1)));
                assert_eq!(request.documents[0], lockfile);
            });
    }

    #[test]
    fn score_order_is_descending_with_a_caller_tie_break() {
        let scores = [0.2, 0.9, 0.9, 0.5];
        let order = order_by_score(&scores, |a, b| a.cmp(&b));
        assert_eq!(order, vec![1, 2, 3, 0]);
    }
}
