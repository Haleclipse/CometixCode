//! The shared System One client (decision-provider.md §1.4, §5).
//!
//! @nyan: System One is the only protocol the decision provider speaks, and
//! every provider of it differs in three small ways, not in its whole shape:
//! endpoint path, request envelope, auth header. So the reusable part lives
//! here — keep-alive client, `User-Agent: nyan/<version>`, Bearer key, health
//! gating, timeout/abort handling, request-ID capture, wire encoding of
//! noul / choice / score, and strict answer parsing — and a new provider only
//! implements [`SystemOneTransport`] (URL + envelope) and a constructor that
//! supplies its key and client. EMRank is the first such provider; TypeSafe /
//! OpenRouter serve the identical protocol and need no new client.
//!
//! Measured behaviour this module is shaped around (corp-pc, 2026-10-27):
//! one request with 20 batched candidates costs about the same as one request
//! with one candidate (0.5–0.6 s), while 20 independent requests cost far
//! more. Hence: keep-alive pool, one request per decision, no retries (a retry
//! cannot fit the 800 ms critical-path budget; the caller's deterministic path
//! is the fallback).
//!
//! Availability goes through the shared health machine: a 429 / 402 / 401 /
//! timeout / connection failure closes the service for a backoff window.
//! While Down nothing is sent; an expired window triggers exactly one
//! background probe and the triggering call still takes the fallback path.
//!
//! Tests never talk to a real provider: the URL, key and HTTP client are
//! constructor arguments, and the tests bind a localhost mock.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::services::decision::{
    Answer, DecisionBackend, DecisionOutcome, DecisionRequest, DecisionUsage, FallbackReason,
    Question, Receipt, StateDigest,
};
use crate::services::service_health::{self, CallPermit, FailureKind, ProbeFn};
use crate::tool::AbortController;

/// `User-Agent: nyan/<version>` — the default reqwest UA is filtered by
/// Cloudflare in front of EMRank (403, error code 1010), so this header is not
/// optional.
pub fn user_agent() -> String {
    format!("nyan/{}", crate::constants::product::VERSION)
}

/// Differences between services that serve System One models.
///
/// Adding a provider means implementing this trait (~20 lines) plus a
/// constructor; no HTTP, parsing, health or timeout code is duplicated.
pub trait SystemOneTransport: Send + Sync {
    /// Wire name recorded in receipts.
    fn name(&self) -> &'static str;

    /// Absolute request URL for a base URL and model.
    fn url(&self, base_url: &str, model: &str) -> String;

    /// The service's request envelope around the System One body.
    fn encode(&self, model: &str, state: &Value, questions: &Map<String, Value>) -> Value;
}

/// EMRank's native System One envelope: the body is the envelope, posted to
/// the configured decision URL.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeSystemOneTransport;

impl SystemOneTransport for NativeSystemOneTransport {
    fn name(&self) -> &'static str {
        "emrank"
    }

    fn url(&self, base_url: &str, _model: &str) -> String {
        base_url.to_string()
    }

    fn encode(&self, model: &str, state: &Value, questions: &Map<String, Value>) -> Value {
        json!({
            "model": model,
            "state": state,
            "questions": questions,
        })
    }
}

/// TypeSafe's native envelope (also served by OpenRouter): same body, model at
/// the top level, posted to `<base>/systemone`. Kept as a reference
/// implementation of the transport seam; no settings entry selects it yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct TypesafeSystemOneTransport;

impl SystemOneTransport for TypesafeSystemOneTransport {
    fn name(&self) -> &'static str {
        "typesafe"
    }

    fn url(&self, base_url: &str, _model: &str) -> String {
        format!("{}/systemone", base_url.trim_end_matches('/'))
    }

    fn encode(&self, model: &str, state: &Value, questions: &Map<String, Value>) -> Value {
        json!({
            "model": model,
            "state": state,
            "questions": questions,
        })
    }
}

pub struct SystemOneClient {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    health: Arc<service_health::ServiceHealth>,
    transport: Arc<dyn SystemOneTransport>,
    /// Model used by the health probe. The service rejects a model id the
    /// account has not enabled; the old hardcoded `nyan-probe` produced a 404
    /// that looked like a dead backend. Callers pass the configured `fast`
    /// alias, which is the model real decisions use.
    probe_model: String,
    prewarmed: AtomicBool,
}

impl std::fmt::Debug for SystemOneClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key.
        f.debug_struct("SystemOneClient")
            .field("base_url", &self.base_url)
            .field("transport", &self.transport.name())
            .finish_non_exhaustive()
    }
}

impl SystemOneClient {
    pub fn with_transport(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
        health: Arc<service_health::ServiceHealth>,
        transport: Arc<dyn SystemOneTransport>,
        probe_model: impl Into<String>,
    ) -> Arc<Self> {
        let backend = Arc::new(Self {
            base_url: base_url.into(),
            // Trim defensively: a key read from a file routinely ends with a
            // newline, and a newline in a header is a builder error.
            api_key: api_key.into().trim().to_string(),
            client,
            health,
            transport,
            probe_model: probe_model.into(),
            prewarmed: AtomicBool::new(false),
        });
        backend.install_probe();
        backend
    }

    /// The health probe is a minimal decision request: System One has no
    /// separate readiness endpoint, and a 200 on the decision path is the
    /// strongest evidence the service is usable.
    fn install_probe(self: &Arc<Self>) {
        let probe = Arc::clone(self);
        let future: ProbeFn = Arc::new(move || {
            let probe = Arc::clone(&probe);
            Box::pin(async move { probe.ping(Duration::from_millis(2_000)).await })
        });
        self.health.set_probe(Some(future));
    }

    /// §5.2: warm the connection once after the first user input. Failures are
    /// silent — a failed warm simply means the first real decision pays the
    /// handshake or falls back.
    pub fn spawn_prewarm(self: &Arc<Self>) {
        if self.prewarmed.swap(true, Ordering::Relaxed) {
            return;
        }
        let backend = Arc::clone(self);
        let future = async move {
            if backend.ping(Duration::from_millis(2_000)).await.is_err() {
                backend.prewarmed.store(false, Ordering::Relaxed);
            }
        };
        if let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
            handle.spawn(future);
        } else if tokio::runtime::Handle::try_current().is_ok() {
            // Tests and embedded callers.
            tokio::spawn(future);
        } else {
            // No runtime: allow a later attempt.
            self.prewarmed.store(false, Ordering::Relaxed);
        }
    }

    async fn ping(&self, timeout: Duration) -> Result<(), ()> {
        let mut questions = Map::new();
        questions.insert(
            "ping".to_string(),
            json!({"type": "noul", "instructions": "Is this a ping?"}),
        );
        let body =
            self.transport
                .encode(&self.probe_model, &Value::String("ping".into()), &questions);
        self.post(&body, timeout).await.map(|_| ()).map_err(|_| ())
    }

    /// One HTTP exchange. The timeout covers connect, send and the full body,
    /// so a caller's 800 ms deadline is a wall-clock bound, not per phase.
    async fn post(&self, body: &Value, timeout: Duration) -> Result<Posted, PostError> {
        let url = self.transport.url(&self.base_url, body_model(body));
        let request = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header(reqwest::header::USER_AGENT, user_agent())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);

        let exchange = async {
            let response = request.send().await.map_err(PostError::transport)?;
            let status = response.status();
            let request_id = request_id(&response);
            let bytes = response.bytes().await.map_err(PostError::transport)?;
            Ok::<_, PostError>((status, request_id, bytes))
        };

        let (status, request_id, bytes) = match tokio::time::timeout(timeout, exchange).await {
            Ok(Ok(parts)) => parts,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                return Err(PostError {
                    reason: FallbackReason::Timeout,
                    failure: FailureKind::Timeout,
                    request_id: None,
                });
            }
        };

        if !status.is_success() {
            let (reason, failure) = classify_failure(status.as_u16(), &bytes);
            // Keep the service's own reason visible: a model id the account
            // has not enabled is a configuration error, not an unreachable
            // backend, and the old mapping hid the difference.
            let detail = error_summary(&bytes);
            crate::utils::debug::log_for_debugging(&format!(
                "DecisionProvider: HTTP {} ({}) for model {:?}{}",
                status.as_u16(),
                reason.as_str(),
                body_model(body),
                detail
                    .as_deref()
                    .map(|detail| format!(": {detail}"))
                    .unwrap_or_default(),
            ));
            return Err(PostError {
                reason,
                failure,
                request_id,
            });
        }

        let body: Value = serde_json::from_slice(&bytes).map_err(|_| PostError {
            reason: FallbackReason::InvalidResponse,
            failure: FailureKind::InvalidResponse,
            request_id: request_id.clone(),
        })?;
        Ok(Posted { body, request_id })
    }
}

fn body_model(body: &Value) -> &str {
    body.get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Map an HTTP failure to a fallback reason and a health failure kind.
///
/// A 404 that names an unknown/disabled model is `ModelNotFound`, not
/// `BackendDown`: the endpoint is healthy, the configuration is not. A 400 is
/// a malformed request and therefore `InvalidResponse`. Everything else keeps
/// the documented 429 / 402 / 401-403 mapping.
fn classify_failure(status: u16, body: &[u8]) -> (FallbackReason, FailureKind) {
    match status {
        429 => (FallbackReason::RateLimited, FailureKind::RateLimited),
        402 => (FallbackReason::BudgetExhausted, FailureKind::Budget),
        401 | 403 => (FallbackReason::BackendDown, FailureKind::Auth),
        // Some deployments answer 400 for an unknown model, some 404.
        400 | 404 | 422 if mentions_model_not_found(body) => {
            (FallbackReason::ModelNotFound, FailureKind::ModelNotFound)
        }
        400 | 422 => (
            FallbackReason::InvalidResponse,
            FailureKind::InvalidResponse,
        ),
        _ => (FallbackReason::BackendDown, FailureKind::Unreachable),
    }
}

/// Whether the service's error payload says the model does not exist or is
/// not enabled. The provider uses both an English code and a Chinese message.
fn mentions_model_not_found(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let error = value.get("error").unwrap_or(&value);
    ["code", "type", "message"]
        .iter()
        .filter_map(|key| error.get(*key))
        .filter_map(Value::as_str)
        .any(|text| {
            let lower = text.to_ascii_lowercase();
            lower.contains("model_not_found")
                || lower.contains("model not found")
                || lower.contains("not enabled")
                || text.contains("模型不存在")
                || text.contains("未启用")
        })
}

/// A short, log-safe summary of the service's error payload. The decision
/// request body is never logged; this only reads the response.
fn error_summary(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    let mut parts = Vec::new();
    for key in ["code", "type", "message"] {
        let Some(value) = error.get(key) else {
            continue;
        };
        let text = match value {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            _ => continue,
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        parts.push(format!("{key}={text}"));
    }
    if parts.is_empty() {
        return None;
    }
    let mut summary = parts.join(" ");
    // `String::truncate` panics off a char boundary; the service's messages
    // are Chinese, so cut at the last boundary at or below 200 bytes.
    if summary.len() > 200 {
        let mut end = 200;
        while !summary.is_char_boundary(end) {
            end -= 1;
        }
        summary.truncate(end);
    }
    Some(summary)
}

struct Posted {
    body: Value,
    request_id: Option<String>,
}

struct PostError {
    reason: FallbackReason,
    failure: FailureKind,
    request_id: Option<String>,
}

impl PostError {
    fn transport(error: reqwest::Error) -> Self {
        // Debug log only: never the key or the body (decision-provider §3.6
        // applies the same rule to provider errors).
        crate::utils::debug::log_for_debugging(&format!(
            "DecisionProvider: transport error: {error}"
        ));
        let timed_out = error.is_timeout();
        Self {
            reason: if timed_out {
                FallbackReason::Timeout
            } else {
                FallbackReason::BackendDown
            },
            failure: if timed_out {
                FailureKind::Timeout
            } else {
                FailureKind::Unreachable
            },
            request_id: None,
        }
    }
}

fn request_id(response: &reqwest::Response) -> Option<String> {
    // The design names `X-Request-ID`; legacy's provider emits
    // `x-typesafe-request-id`. Accept either.
    for name in ["x-request-id", "x-typesafe-request-id"] {
        if let Some(value) = response.headers().get(name)
            && let Ok(value) = value.to_str()
        {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// System One wire question. The public noul type is `noul` on the wire
/// (legacy maps its public `bool` type to it).
fn wire_question(question: &Question) -> Value {
    match question {
        Question::Noul { instructions } => json!({
            "type": "noul",
            "instructions": instructions,
        }),
        Question::Choice {
            instructions,
            options,
        } => json!({
            "type": "choice",
            "instructions": instructions,
            "criteria": options,
        }),
        Question::Score {
            instructions,
            levels,
        } => json!({
            "type": "score",
            "instructions": instructions,
            "criteria": levels,
        }),
    }
}

fn finite_probability(value: Option<&Value>) -> Result<f64, ()> {
    let Some(number) = value.and_then(Value::as_f64) else {
        return Err(());
    };
    if number.is_finite() && (0.0..=1.0).contains(&number) {
        Ok(number)
    } else {
        Err(())
    }
}

/// Parse one answer against the question that produced it. A type mismatch,
/// a missing field, a non-finite number, an out-of-range probability or an
/// unknown choice/level is `invalid_response` — the whole batch is discarded.
fn parse_answer(question: &Question, value: &Value) -> Result<Answer, ()> {
    let ty = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match question {
        Question::Noul { .. } => {
            if ty != "noul" {
                return Err(());
            }
            Ok(Answer::Noul {
                probability: finite_probability(value.get("noul"))?,
            })
        }
        Question::Choice { options, .. } => {
            if ty != "choice" {
                return Err(());
            }
            let choice = value
                .get("choice")
                .and_then(Value::as_str)
                .ok_or(())?
                .to_string();
            if !options.contains_key(&choice) {
                return Err(());
            }
            let confidence = finite_probability(value.get("confidence"))?;
            let mut probabilities = BTreeMap::new();
            if let Some(map) = value.get("probabilities").and_then(Value::as_object) {
                for (id, raw) in map {
                    let p = raw.as_f64().ok_or(())?;
                    if !p.is_finite() || !(0.0..=1.0).contains(&p) {
                        return Err(());
                    }
                    probabilities.insert(id.clone(), p);
                }
            }
            Ok(Answer::Choice {
                choice,
                probabilities,
                confidence,
            })
        }
        Question::Score { levels, .. } => {
            if ty != "score" {
                return Err(());
            }
            let score = value.get("score").and_then(Value::as_f64).ok_or(())?;
            if !score.is_finite() || score < 0.0 || score.fract() != 0.0 {
                return Err(());
            }
            let level = score as u64;
            if level >= levels.len() as u64 {
                return Err(());
            }
            let confidence = finite_probability(value.get("confidence"))?;
            Ok(Answer::Score {
                level: level as u32,
                confidence,
            })
        }
    }
}

fn parse_usage(response: &Map<String, Value>) -> Option<DecisionUsage> {
    let usage = response.get("usage")?.as_object()?;
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if input == 0 && output == 0 {
        return None;
    }
    Some(DecisionUsage {
        input_tokens: input,
        output_tokens: output,
    })
}

/// A receipt carrying only service-reported values plus the state digest.
fn receipt(
    backend: &str,
    req: &DecisionRequest,
    model: &str,
    request_id: Option<String>,
    latency: Duration,
    usage: Option<DecisionUsage>,
    digest: &StateDigest,
) -> Receipt {
    Receipt {
        point: req.point.as_str().to_string(),
        backend: backend.to_string(),
        model: model.to_string(),
        latency,
        usage,
        request_id,
        answers: BTreeMap::new(),
        agreement: BTreeMap::new(),
        state: digest.clone(),
    }
}

impl DecisionBackend for SystemOneClient {
    fn decide<'a>(
        &'a self,
        req: &'a DecisionRequest,
        deadline: Duration,
        abort: &'a AbortController,
    ) -> crate::services::decision::BackendFuture<'a> {
        Box::pin(async move {
            let digest = crate::services::decision::digest_state(&req.state);
            let backend_name = self.transport.name();
            let fallback = |reason: FallbackReason, request_id: Option<String>, latency| {
                DecisionOutcome::Fallback {
                    reason,
                    receipt: receipt(
                        backend_name,
                        req,
                        &req.model,
                        request_id,
                        latency,
                        None,
                        &digest,
                    ),
                }
            };

            // Down: do not open a socket. An expired window triggers exactly
            // one background probe; this call still takes the fallback path.
            match self.health.call_permit() {
                CallPermit::Skipped => {
                    return fallback(FallbackReason::BackendDown, None, Duration::ZERO);
                }
                CallPermit::Probe => {
                    self.health.spawn_probe();
                    return fallback(FallbackReason::BackendDown, None, Duration::ZERO);
                }
                CallPermit::Allowed => {}
            }

            if abort.is_aborted() {
                return fallback(FallbackReason::Cancelled, None, Duration::ZERO);
            }

            let questions: Map<String, Value> = req
                .questions
                .iter()
                .map(|(id, question)| (id.clone(), wire_question(question)))
                .collect();
            let body = self.transport.encode(&req.model, &req.state, &questions);

            let started = Instant::now();
            let posted = tokio::select! {
                result = self.post(&body, deadline) => result,
                _ = wait_aborted(abort) => {
                    return fallback(FallbackReason::Cancelled, None, started.elapsed());
                }
            };
            let latency = started.elapsed();

            let posted = match posted {
                Ok(posted) => posted,
                Err(error) => {
                    self.health.record_failure(error.failure);
                    crate::utils::debug::log_for_debugging(&format!(
                        "DecisionProvider: {} {} -> {} after {} ms",
                        backend_name,
                        req.point.as_str(),
                        error.reason.as_str(),
                        latency.as_millis()
                    ));
                    return fallback(error.reason, error.request_id, latency);
                }
            };

            let invalid = |request_id: Option<String>| {
                self.health.record_failure(FailureKind::InvalidResponse);
                DecisionOutcome::Fallback {
                    reason: FallbackReason::InvalidResponse,
                    receipt: receipt(
                        backend_name,
                        req,
                        &req.model,
                        request_id,
                        latency,
                        None,
                        &digest,
                    ),
                }
            };
            let Some(response) = posted.body.as_object() else {
                return invalid(posted.request_id);
            };
            let response_model = response
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&req.model)
                .to_string();
            let Some(answer_values) = response.get("answers").and_then(Value::as_object) else {
                return invalid(posted.request_id);
            };

            let mut answers = BTreeMap::new();
            for (id, question) in &req.questions {
                let Some(value) = answer_values.get(id) else {
                    return invalid(posted.request_id);
                };
                match parse_answer(question, value) {
                    Ok(answer) => {
                        answers.insert(id.clone(), answer);
                    }
                    Err(()) => return invalid(posted.request_id),
                }
            }

            self.health.record_success();
            let usage = parse_usage(response);
            // Logging keeps only the digest and counts (decision-provider §7).
            crate::utils::debug::log_for_debugging(&format!(
                "DecisionProvider: {} {} answered {} question(s) in {} ms (model {response_model}, request {}, state {}…/{} bytes)",
                backend_name,
                req.point.as_str(),
                answers.len(),
                latency.as_millis(),
                posted.request_id.as_deref().unwrap_or("-"),
                &digest.sha256[..12.min(digest.sha256.len())],
                digest.bytes,
            ));
            let mut receipt = receipt(
                backend_name,
                req,
                &response_model,
                posted.request_id,
                latency,
                usage,
                &digest,
            );
            receipt.answers = answers.clone();
            DecisionOutcome::Answered { answers, receipt }
        })
    }

    fn name(&self) -> &'static str {
        self.transport.name()
    }
}

/// Resolves when the caller's abort flag flips. Polling keeps this module free
/// of a second cancellation channel; the interval is far below the 800 ms
/// critical-path budget.
async fn wait_aborted(abort: &AbortController) {
    loop {
        if abort.is_aborted() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::decision::DecisionPoint;
    use crate::services::service_health::{ServiceHealth, ServiceKind};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A localhost mock that answers each connection with the next queued
    /// response and returns the captured requests when joined.
    struct Mock {
        url: String,
        handle: tokio::task::JoinHandle<Vec<String>>,
    }

    async fn mock(responses: Vec<(u16, &'static str)>) -> Mock {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut captured = Vec::new();
            for (status, reason) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = Vec::new();
                let header_end = loop {
                    let mut chunk = [0u8; 4096];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0, "request ended before headers");
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
                    assert_ne!(count, 0, "request ended before body");
                    buffer.extend_from_slice(&chunk[..count]);
                }
                let body = String::from_utf8_lossy(&buffer[header_end..]).to_string();
                captured.push(format!("{headers}{body}"));
                let body = reason_body(status);
                let reply = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\nx-request-id: req_test_123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
            captured
        });
        Mock {
            url: format!("http://{address}"),
            handle,
        }
    }

    fn reason_body(status: u16) -> &'static str {
        match status {
            200 => {
                r#"{"model":"typesafe/jev-1.14.0","answers":{"answers":{"type":"noul","noul":0.91}},"usage":{"input_tokens":210,"output_tokens":3}}"#
            }
            // Matches the account's real 404 for `typesafe/jev-1.14.0`.
            404 => {
                r#"{"error":{"message":"模型不存在或未启用","type":"model_not_found","code":"model_not_found"}}"#
            }
            _ => "{}",
        }
    }

    fn client(url: &str) -> Arc<SystemOneClient> {
        crate::utils::tls_provider::install_crypto_provider();
        SystemOneClient::with_transport(
            url,
            "test-key\n",
            reqwest::Client::builder().no_proxy().build().unwrap(),
            Arc::new(ServiceHealth::with_timeouts(
                ServiceKind::Emrank,
                Duration::from_millis(500),
                Duration::from_millis(2_000),
            )),
            Arc::new(NativeSystemOneTransport),
            "typesafe/jev-1.14.0",
        )
    }

    fn request() -> DecisionRequest {
        let mut questions = BTreeMap::new();
        questions.insert(
            "answers".to_string(),
            Question::noul("Does this code answer the query?"),
        );
        DecisionRequest {
            point: DecisionPoint::RetrievalAnswers,
            model: "typesafe/jev-1.14.0".to_string(),
            state: json!({"query": "where is auth handled?", "code": "fn auth() {}"}),
            questions,
        }
    }

    async fn run(backend: &SystemOneClient, request: &DecisionRequest) -> DecisionOutcome {
        backend
            .decide(
                request,
                Duration::from_millis(2_000),
                &AbortController::default(),
            )
            .await
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn a_noul_answer_is_parsed_and_the_wire_request_carries_ua_and_bearer() {
        runtime().block_on(async {
            let mock = mock(vec![(200, "OK")]).await;
            let backend = client(&mock.url);
            let outcome = run(&backend, &request()).await;
            let DecisionOutcome::Answered { answers, receipt } = outcome else {
                panic!("fallback: {:?}", outcome.fallback_reason());
            };
            assert_eq!(answers["answers"], Answer::Noul { probability: 0.91 });
            assert_eq!(receipt.request_id.as_deref(), Some("req_test_123"));
            assert_eq!(receipt.model, "typesafe/jev-1.14.0");
            assert_eq!(receipt.backend, "emrank");
            assert_eq!(receipt.usage.unwrap().input_tokens, 210);
            assert!(receipt.state.bytes > 0);

            let captured = mock.handle.await.unwrap();
            let raw = &captured[0];
            assert!(raw.to_lowercase().contains("user-agent: nyan/"), "{raw}");
            assert!(
                raw.to_lowercase()
                    .contains("authorization: bearer test-key"),
                "{raw}"
            );
            assert!(!raw.contains("test-key\n"), "key must be trimmed");
            assert!(raw.contains("\"type\":\"noul\""), "{raw}");
            assert!(raw.contains("\"model\":\"typesafe/jev-1.14.0\""), "{raw}");
            assert!(raw.contains("where is auth handled?"), "{raw}");
        });
    }

    #[test]
    fn choice_and_score_answers_are_parsed_and_batched_in_one_request() {
        runtime().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let handle = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = Vec::new();
                loop {
                    let mut chunk = [0u8; 4096];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0);
                    buffer.extend_from_slice(&chunk[..count]);
                    if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&buffer[..end]).to_string();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().ok())?
                            })
                            .unwrap_or(0);
                        if buffer.len() - (end + 4) >= length {
                            break;
                        }
                    }
                }
                let body = r#"{"model":"typesafe/jev-1.14.0","answers":{"kind":{"type":"choice","choice":"callers","probabilities":{"callers":0.8,"literal":0.2},"confidence":0.9},"level":{"type":"score","score":2,"confidence":0.7}}}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
                String::from_utf8_lossy(&buffer).to_string()
            });
            let backend = client(&format!("http://{address}"));
            let mut questions = BTreeMap::new();
            questions.insert(
                "kind".to_string(),
                Question::choice(
                    "What kind of query is this?",
                    &[("callers", "who calls it"), ("literal", "literal text")],
                ),
            );
            questions.insert(
                "level".to_string(),
                Question::score("How relevant?", &["none", "partial", "direct"]),
            );
            let request = DecisionRequest {
                point: DecisionPoint::RetrievalIntent,
                model: "typesafe/jev-1.14.0".to_string(),
                state: json!({"query": "who calls auth"}),
                questions,
            };
            let outcome = run(&backend, &request).await;
            let DecisionOutcome::Answered { answers, .. } = outcome else {
                panic!("fallback: {:?}", outcome.fallback_reason());
            };
            assert_eq!(
                answers["kind"],
                Answer::Choice {
                    choice: "callers".into(),
                    probabilities: [
                        ("callers".to_string(), 0.8),
                        ("literal".to_string(), 0.2)
                    ]
                    .into_iter()
                    .collect(),
                    confidence: 0.9,
                }
            );
            assert_eq!(
                answers["level"],
                Answer::Score {
                    level: 2,
                    confidence: 0.7
                }
            );
            let raw = handle.await.unwrap();
            assert!(raw.contains("\"type\":\"choice\""), "{raw}");
            assert!(raw.contains("\"type\":\"score\""), "{raw}");
            // Both questions travelled in the same request body.
            assert_eq!(raw.matches("\"instructions\"").count(), 2, "{raw}");
        });
    }

    #[test]
    fn http_429_402_and_401_map_to_the_documented_fallback_reasons() {
        for (status, reason, expected) in [
            (429u16, "Too Many Requests", FallbackReason::RateLimited),
            (402, "Payment Required", FallbackReason::BudgetExhausted),
            (401, "Unauthorized", FallbackReason::BackendDown),
        ] {
            runtime().block_on(async {
                let mock = mock(vec![(status, reason)]).await;
                let backend = client(&mock.url);
                let outcome = run(&backend, &request()).await;
                assert_eq!(outcome.fallback_reason(), Some(expected), "status {status}");
                // The service is now Down; a second call must not reach the
                // mock (which would panic on a missing accept).
                let second = run(&backend, &request()).await;
                assert_eq!(second.fallback_reason(), Some(FallbackReason::BackendDown));
                mock.handle.await.unwrap();
            });
        }
    }

    #[test]
    fn a_malformed_response_is_invalid_and_marks_the_service_down() {
        runtime().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let handle = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut chunk = [0u8; 4096];
                let _ = stream.read(&mut chunk).await.unwrap();
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 8\r\nconnection: close\r\n\r\nnot json",
                    )
                    .await
                    .unwrap();
            });
            let backend = client(&format!("http://{address}"));
            let outcome = run(&backend, &request()).await;
            assert_eq!(
                outcome.fallback_reason(),
                Some(FallbackReason::InvalidResponse)
            );
            handle.await.unwrap();
        });
    }

    #[test]
    fn an_out_of_range_probability_or_unknown_choice_is_invalid() {
        for (body, expect_answer) in [
            (
                r#"{"answers":{"answers":{"type":"noul","noul":1.5}}}"#,
                false,
            ),
            (
                r#"{"answers":{"answers":{"type":"noul","noul":-0.1}}}"#,
                false,
            ),
            (
                r#"{"answers":{"answers":{"type":"choice","choice":"x"}}}"#,
                false,
            ),
            (r#"{"answers":{}}"#, false),
            // Malformed usage is optional: the answer still counts.
            (
                r#"{"answers":{"answers":{"type":"noul","noul":0.9}},"usage":{"input_tokens":"many"}}"#,
                true,
            ),
        ] {
            runtime().block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let handle = tokio::spawn({
                    let body = body.to_string();
                    async move {
                        let (mut stream, _) = listener.accept().await.unwrap();
                        let mut chunk = [0u8; 4096];
                        let _ = stream.read(&mut chunk).await.unwrap();
                        let reply = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(reply.as_bytes()).await;
                    }
                });
                let backend = client(&format!("http://{address}"));
                let outcome = run(&backend, &request()).await;
                if expect_answer {
                    assert!(outcome.answers().is_some(), "{body}");
                } else {
                    assert_eq!(
                        outcome.fallback_reason(),
                        Some(FallbackReason::InvalidResponse),
                        "{body}"
                    );
                }
                handle.await.unwrap();
            });
        }
    }

    #[test]
    fn a_timeout_maps_to_timeout_and_opens_the_backoff_window() {
        runtime().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            // Accept but never answer.
            let handle = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::time::sleep(Duration::from_secs(5)).await;
                drop(stream);
            });
            let backend = client(&url);
            let outcome = tokio::time::timeout(
                Duration::from_secs(2),
                backend.decide(
                    &request(),
                    Duration::from_millis(100),
                    &AbortController::default(),
                ),
            )
            .await
            .expect("must respect the deadline");
            assert_eq!(outcome.fallback_reason(), Some(FallbackReason::Timeout));
            assert!(backend.health.is_down());
            handle.abort();
        });
    }

    #[test]
    fn the_health_probe_uses_the_configured_model() {
        runtime().block_on(async {
            let mock = mock(vec![(200, "OK")]).await;
            let backend = client(&mock.url);
            backend.ping(Duration::from_secs(1)).await.unwrap();
            let raw = mock.handle.await.unwrap();
            assert!(
                raw[0].contains("\"model\":\"typesafe/jev-1.14.0\""),
                "probe must use the configured model: {}",
                raw[0]
            );
            assert!(!raw[0].contains("nyan-probe"), "{}", raw[0]);
        });
    }

    #[test]
    fn an_unknown_model_is_model_not_found_not_backend_down() {
        runtime().block_on(async {
            let mock = mock(vec![(404, "Not Found")]).await;
            let backend = client(&mock.url);
            let outcome = run(&backend, &request()).await;
            assert_eq!(
                outcome.fallback_reason(),
                Some(FallbackReason::ModelNotFound)
            );
            assert_eq!(
                backend.health.last_failure(),
                Some(FailureKind::ModelNotFound),
                "the last failure must name the real cause"
            );
            mock.handle.await.unwrap();
        });
    }

    #[test]
    fn a_long_multibyte_error_is_summarized_without_panicking() {
        // This shape puts byte 200 inside a Chinese character: the old
        // `String::truncate(200)` panicked here (release builds abort).
        let body = json!({
            "error": {
                "code": "400",
                "type": "E",
                "message": "模型不存在或未启用".repeat(30),
            }
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let summary = error_summary(&bytes).expect("summary");
        assert!(summary.len() <= 200, "{} bytes", summary.len());
        assert!(summary.starts_with("code=400 type=E message=模型"));
    }

    #[test]
    fn a_down_service_is_skipped_without_reaching_the_network() {
        runtime().block_on(async {
            crate::utils::tls_provider::install_crypto_provider();
            let backend = SystemOneClient::with_transport(
                "http://127.0.0.1:1",
                "k",
                reqwest::Client::builder().no_proxy().build().unwrap(),
                Arc::new(ServiceHealth::with_timeouts(
                    ServiceKind::Emrank,
                    Duration::from_millis(50),
                    Duration::from_millis(50),
                )),
                Arc::new(NativeSystemOneTransport),
                "typesafe/jev-1.14.0",
            );
            backend.health.record_failure(FailureKind::Unreachable);
            let started = Instant::now();
            let outcome = run(&backend, &request()).await;
            assert_eq!(outcome.fallback_reason(), Some(FallbackReason::BackendDown));
            assert!(
                started.elapsed() < Duration::from_millis(100),
                "must not wait for a connection"
            );
        });
    }

    #[test]
    fn an_aborted_caller_cancels_before_the_request_completes() {
        runtime().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let handle = tokio::spawn(async move {
                let (_stream, _) = listener.accept().await.unwrap();
                tokio::time::sleep(Duration::from_secs(5)).await;
            });
            let backend = client(&url);
            let abort = AbortController::default();
            let cancel = abort.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                cancel.abort();
            });
            let outcome = backend
                .decide(&request(), Duration::from_secs(2), &abort)
                .await;
            assert_eq!(outcome.fallback_reason(), Some(FallbackReason::Cancelled));
            handle.abort();
        });
    }

    #[test]
    fn a_transport_controls_only_url_and_envelope() {
        let typesafe = TypesafeSystemOneTransport;
        assert_eq!(
            typesafe.url("https://api.typesafe.ai/v1/", "m"),
            "https://api.typesafe.ai/v1/systemone"
        );
        let mut questions = Map::new();
        questions.insert("q".into(), json!({"type": "noul"}));
        let body = typesafe.encode("m", &json!({"text": "x"}), &questions);
        assert_eq!(body["model"], "m");
        assert_eq!(body["state"]["text"], "x");

        // The native transport posts the configured URL unchanged.
        assert_eq!(
            NativeSystemOneTransport.url("https://emrank.tech/v1/decisions", "m"),
            "https://emrank.tech/v1/decisions"
        );
    }

    #[test]
    fn the_user_agent_is_nyan_versioned() {
        assert_eq!(
            user_agent(),
            format!("nyan/{}", crate::constants::product::VERSION)
        );
    }
}
