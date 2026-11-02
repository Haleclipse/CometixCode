//! Path-routed local HTTP mock for fetcher / fallback / tool tests.
//!
//! @nyan: test-only. Serves plain HTTP on `127.0.0.1:<random>`; the fetcher's
//! test seam (`allow_private_for_tests`) is what lets it reach this address.
//! Absolute-form request targets (what an HTTP proxy receives) are routed by
//! their path, so the same server can stand in for a proxy.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::services::decision::{
    Answer, BackendFuture, DecisionBackend, DecisionOutcome, DecisionProvider, DecisionRequest,
    FallbackReason, Receipt,
};
use crate::tool::AbortController;
use crate::utils::settings::nyan::DecisionSettings;

/// Installs `nyan.web` into the merged settings for one test. Holds the env
/// lock so settings caches cannot be reset underneath it; dropping it resets
/// them. Decision points are disabled unless the test installs a scripted
/// provider, so no test can reach a real backend through global settings.
pub(crate) struct WebSettingsGuard {
    _lock: crate::utils::test_env::TestEnvGuard<'static>,
}

impl WebSettingsGuard {
    /// No `nyan` block at all (the CC-fallback configuration).
    pub fn none() -> Self {
        use crate::utils::settings::settings_cache;
        use crate::utils::settings::validation::SettingsWithErrors;

        let lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        settings_cache::set_session_settings_cache(SettingsWithErrors {
            settings: crate::utils::settings::types::SettingsJson::default(),
            ..Default::default()
        });
        Self { _lock: lock }
    }

    pub fn install(web: crate::utils::settings::nyan::WebSettings) -> Self {
        use crate::utils::settings::settings_cache;
        use crate::utils::settings::validation::SettingsWithErrors;

        let lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let settings = crate::utils::settings::types::SettingsJson {
            nyan: Some(crate::utils::settings::nyan::NyanSettings {
                web: Some(web),
                decision: Some(disabled_decisions()),
                ..Default::default()
            }),
            ..Default::default()
        };
        settings_cache::set_session_settings_cache(SettingsWithErrors {
            settings,
            ..Default::default()
        });
        Self { _lock: lock }
    }
}

/// Routes `decision::provider()` to a scripted backend for one test, so tool
/// paths exercise the real seam without a key or a network.
pub(crate) struct DecisionOverrideGuard;

impl DecisionOverrideGuard {
    pub fn install(provider: DecisionProvider) -> Self {
        super::decision::test_override::set(std::sync::Arc::new(provider));
        Self
    }
}

impl Drop for DecisionOverrideGuard {
    fn drop(&mut self) {
        super::decision::test_override::clear();
    }
}

fn disabled_decisions() -> crate::utils::settings::nyan::DecisionSettings {
    let mut settings = crate::utils::settings::nyan::DecisionSettings::default();
    settings.enabled = Some(false);
    settings
}

/// Tests name the providers they mock. Every other builtin starts disabled so
/// it is not built against the real endpoint: production loads a builtin the
/// user did not mention, and the only opt-out is `enabled: false`.
pub(crate) fn only_mocked_providers(partial: Value) -> Value {
    let mut providers = serde_json::Map::new();
    for name in crate::utils::settings::nyan::provider_names() {
        providers.insert(name.to_string(), json!({"enabled": false}));
    }
    if let Some(given) = partial.as_object() {
        for (name, config) in given {
            providers.insert(name.clone(), config.clone());
        }
    }
    Value::Object(providers)
}

/// A key file with a fake key, shared by the process. Providers refuse to send
/// without a key, so mock-backed provider tests point `apiKeyFile` here.
pub(crate) fn key_file() -> String {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("nyan-test-keys-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key");
        std::fs::write(&path, "test-key-not-real\n").unwrap();
        path
    })
    .to_string_lossy()
    .into_owned()
}

/// A Decision backend that answers from a closure and records every request.
pub(crate) struct ScriptedBackend {
    script: Box<dyn Fn(&DecisionRequest) -> BTreeMap<String, Answer> + Send + Sync>,
    failure: Option<FallbackReason>,
    requests: Mutex<Vec<DecisionRequest>>,
}

impl ScriptedBackend {
    pub fn provider(
        script: impl Fn(&DecisionRequest) -> BTreeMap<String, Answer> + Send + Sync + 'static,
    ) -> (DecisionProvider, Arc<Self>) {
        Self::provider_with(|_| {}, script)
    }

    pub fn provider_with(
        configure: impl FnOnce(&mut DecisionSettings),
        script: impl Fn(&DecisionRequest) -> BTreeMap<String, Answer> + Send + Sync + 'static,
    ) -> (DecisionProvider, Arc<Self>) {
        let backend = Arc::new(Self {
            script: Box::new(script),
            failure: None,
            requests: Mutex::new(Vec::new()),
        });
        (Self::wrap(configure, &backend), backend)
    }

    /// A backend that always reports `reason` (timeout, rate limit, ...).
    pub fn failing(reason: FallbackReason) -> (DecisionProvider, Arc<Self>) {
        let backend = Arc::new(Self {
            script: Box::new(|_| BTreeMap::new()),
            failure: Some(reason),
            requests: Mutex::new(Vec::new()),
        });
        (Self::wrap(|_| {}, &backend), backend)
    }

    fn wrap(
        configure: impl FnOnce(&mut DecisionSettings),
        backend: &Arc<Self>,
    ) -> DecisionProvider {
        let mut settings = DecisionSettings::default();
        configure(&mut settings);
        DecisionProvider::with_backend(settings, Arc::clone(backend) as Arc<dyn DecisionBackend>)
    }

    pub fn requests(&self) -> Vec<DecisionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl DecisionBackend for ScriptedBackend {
    fn decide<'a>(
        &'a self,
        req: &'a DecisionRequest,
        _deadline: Duration,
        _abort: &'a AbortController,
    ) -> BackendFuture<'a> {
        self.requests.lock().unwrap().push(req.clone());
        let outcome = match self.failure {
            Some(reason) => DecisionOutcome::Fallback {
                reason,
                receipt: Receipt::default(),
            },
            None => {
                let answers = (self.script)(req);
                DecisionOutcome::Answered {
                    receipt: Receipt {
                        backend: "scripted".to_string(),
                        model: req.model.clone(),
                        answers: answers.clone(),
                        ..Default::default()
                    },
                    answers,
                }
            }
        };
        Box::pin(async move { outcome })
    }

    fn name(&self) -> &'static str {
        "scripted"
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub target: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone)]
pub(crate) struct Route {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay: Duration,
}

impl Route {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_string(), content_type.to_string())],
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn html(body: &str) -> Self {
        Self::new(200, "text/html; charset=utf-8", body)
    }

    pub fn text(body: &str) -> Self {
        Self::new(200, "text/plain", body)
    }

    pub fn redirect(status: u16, location: &str) -> Self {
        let mut route = Self::new(status, "text/plain", "");
        route
            .headers
            .push(("location".to_string(), location.to_string()));
        route
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

pub(crate) struct Server {
    pub origin: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn hits(&self, path: &str) -> usize {
        self.seen().iter().filter(|seen| seen.path == path).count()
    }
}

pub(crate) async fn serve(routes: Vec<(&'static str, Route)>) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            let routes = Arc::clone(&routes);
            tokio::spawn(async move {
                let Some(request) = read_request(&mut stream).await else {
                    return;
                };
                let path = request.path.clone();
                log.lock().unwrap().push(request);
                let route = routes
                    .iter()
                    .find(|(route_path, _)| *route_path == path)
                    .map(|(_, route)| route.clone())
                    .unwrap_or_else(|| Route::new(404, "text/plain", "not found"));
                tokio::time::sleep(route.delay).await;
                let mut head = format!(
                    "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                    route.status,
                    route.body.len()
                );
                for (name, value) in &route.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&route.body).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Server {
        origin: format!("http://{address}"),
        seen,
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<Seen> {
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
    let target = lines.next()?.split_whitespace().nth(1)?.to_string();
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
    Some(Seen {
        path: path_of(&target),
        target,
        headers,
        body: String::from_utf8_lossy(&buffer[header_end..]).to_string(),
    })
}

/// `http://host:port/a?b` and `/a?b` both route as `/a`.
fn path_of(target: &str) -> String {
    let path = match target.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |index| &rest[index..]),
        None => target,
    };
    path.split('?').next().unwrap_or("/").to_string()
}
