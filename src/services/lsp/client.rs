//! Stdio JSON-RPC LSP client runtime.
//!
//! Maps to: CC `services/lsp/LSPClient.ts` `createLSPClient(...)`.
//!
//! Claude Code uses `vscode-jsonrpc` to frame Language Server Protocol
//! messages over child-process stdio. This Rust port keeps the same file
//! boundary and responsibilities: process start/stop, initialize, request and
//! notification transport, queued handler registration, and server-to-client
//! request handling. Higher-level server lifecycle and retry semantics remain
//! in `server_instance.rs`, matching CC `LSPServerInstance.ts`.
//!
//! Ownership shape: CC's `createLSPClient(...)` returns an object whose state
//! lives in closure variables, so *every* method reaches the same state and no
//! caller ever holds it exclusively. This port mirrors that with interior
//! mutability — every method takes `&self`, transport state sits in
//! [`LspClientShared`], and only the child process plus its reader/writer task
//! handles (which just `start`/`stop` touch) live behind a lock. Modelling the
//! client as `&mut self` instead is what forced the owning layers into the
//! `take()`/put-back pattern that made a concurrent LSP caller observe an empty
//! manager slot.
//!
//! Declared deviation (no CC counterpart): the child exit code is not read.
//! CC classifies a crash from `process.on('exit')` with `code !== 0`
//! (`LSPClient.ts:156-167`); this port classifies it from the transport (stdout
//! EOF or a stdin write failure) while no intentional stop is in flight, so a
//! server that exits 0 unprompted is reported as a crash too.

use anyhow::{Context, anyhow};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;

const JSONRPC_VERSION: &str = "2.0";
const MAX_HEADER_BYTES: usize = 64 * 1024;

type NotificationHandler = Arc<dyn Fn(Value) + Send + Sync + 'static>;
type RequestHandler = Arc<dyn Fn(Value) -> Value + Send + Sync + 'static>;
/// Maps to: CC `createLSPClient(serverName, onCrash?)` second parameter
/// (`LSPClient.ts:51-54`).
pub type LspCrashHandler = Arc<dyn Fn(String) + Send + Sync + 'static>;

type PendingRequests = Mutex<HashMap<i64, oneshot::Sender<Result<Value, JsonRpcResponseError>>>>;

/// Maps to: CC `LSPClient.start(...)` `options`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LspClientStartOptions {
    pub env: Option<BTreeMap<String, String>>,
    pub cwd: Option<String>,
}

/// JSON-RPC response error surfaced by `sendRequest(...)`.
/// Maps to the `vscode-jsonrpc` error object used by CC
/// `LSPServerInstance.ts` transient error retry checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonRpcResponseError {
    pub code: Option<i64>,
    pub message: String,
    pub data: Option<Value>,
}

impl fmt::Display for JsonRpcResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(f, "JSON-RPC error {code}: {}", self.message),
            None => write!(f, "JSON-RPC error: {}", self.message),
        }
    }
}

impl std::error::Error for JsonRpcResponseError {}

/// The half of CC's `createLSPClient` closure that every method reads: it is
/// reachable from any caller at any time, including while another request is in
/// flight, so nothing here may be behind an exclusive borrow.
struct LspClientShared {
    server_name: String,
    capabilities: Mutex<Option<Value>>,
    is_initialized: AtomicBool,
    writer: Mutex<Option<mpsc::Sender<Value>>>,
    next_id: AtomicI64,
    crash_error: Mutex<Option<String>>,
    pending_requests: PendingRequests,
    notification_handlers: Mutex<HashMap<String, Vec<NotificationHandler>>>,
    request_handlers: Mutex<HashMap<String, RequestHandler>>,
    start_failed: AtomicBool,
    start_error: Mutex<Option<String>>,
    /// Maps to CC `isStopping` (`LSPClient.ts:62`): suppresses crash reporting
    /// while an intentional shutdown is in flight.
    is_stopping: AtomicBool,
    on_crash: Option<LspCrashHandler>,
}

/// Child process and its pumps. Only `start`/`stop` touch these — the two
/// lifecycle methods CC also serialises through the owning state machine.
#[derive(Default)]
struct LspClientProcess {
    child: Option<Child>,
    writer_task: Option<JoinHandle<()>>,
    reader_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
}

/// Maps to: CC `LSPClient` interface.
pub struct LspClient {
    shared: Arc<LspClientShared>,
    process: AsyncMutex<LspClientProcess>,
}

impl fmt::Debug for LspClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LspClient")
            .field("server_name", &self.shared.server_name)
            .field("capabilities", &self.capabilities())
            .field(
                "is_initialized",
                &self.shared.is_initialized.load(Ordering::SeqCst),
            )
            .field("has_writer", &self.shared.writer.lock().unwrap().is_some())
            .field("next_id", &self.shared.next_id.load(Ordering::SeqCst))
            .field("crash_error", &self.crash_error())
            .field(
                "start_failed",
                &self.shared.start_failed.load(Ordering::SeqCst),
            )
            .field("start_error", &self.shared.start_error.lock().unwrap())
            .field(
                "is_stopping",
                &self.shared.is_stopping.load(Ordering::SeqCst),
            )
            .finish()
    }
}

/// Maps to: CC `createLSPClient(serverName, onCrash?)`. `on_crash` fires the
/// moment the transport dies, so the owning `LspServerInstance` flips to
/// `error` at crash time instead of on the next failed request.
pub fn create_lsp_client(
    server_name: impl Into<String>,
    on_crash: Option<LspCrashHandler>,
) -> LspClient {
    LspClient::new(server_name, on_crash)
}

impl LspClient {
    /// Maps to: CC `createLSPClient(...)` closure state initialization.
    pub fn new(server_name: impl Into<String>, on_crash: Option<LspCrashHandler>) -> Self {
        Self {
            shared: Arc::new(LspClientShared {
                server_name: server_name.into(),
                capabilities: Mutex::new(None),
                is_initialized: AtomicBool::new(false),
                writer: Mutex::new(None),
                next_id: AtomicI64::new(1),
                crash_error: Mutex::new(None),
                pending_requests: Mutex::new(HashMap::new()),
                notification_handlers: Mutex::new(HashMap::new()),
                request_handlers: Mutex::new(HashMap::new()),
                start_failed: AtomicBool::new(false),
                start_error: Mutex::new(None),
                is_stopping: AtomicBool::new(false),
                on_crash,
            }),
            process: AsyncMutex::new(LspClientProcess::default()),
        }
    }

    fn server_name(&self) -> &str {
        &self.shared.server_name
    }

    /// Maps to: CC `LSPClient.capabilities` getter.
    pub fn capabilities(&self) -> Option<Value> {
        self.shared.capabilities.lock().unwrap().clone()
    }

    /// Maps to: CC `LSPClient.isInitialized` getter.
    pub fn is_initialized(&self) -> bool {
        self.shared.is_initialized.load(Ordering::SeqCst) && self.crash_error().is_none()
    }

    /// Maps to: CC `createLSPClient(..., onCrash)` crash propagation state.
    pub fn crash_error(&self) -> Option<String> {
        self.shared.crash_error.lock().unwrap().clone()
    }

    fn clear_crash_error(&self) {
        *self.shared.crash_error.lock().unwrap() = None;
    }

    fn check_start_failed(&self) -> anyhow::Result<()> {
        if let Some(error) = self.crash_error() {
            anyhow::bail!(error);
        }
        if self.shared.start_failed.load(Ordering::SeqCst) {
            anyhow::bail!(
                self.shared
                    .start_error
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| format!(
                        "LSP server {} failed to start",
                        self.server_name()
                    ))
            );
        }
        Ok(())
    }

    /// Maps to: CC `LSPClient.start(command, args, options)`.
    pub async fn start(
        &self,
        command: &str,
        args: &[String],
        options: Option<LspClientStartOptions>,
    ) -> anyhow::Result<()> {
        if self.shared.writer.lock().unwrap().is_some() {
            if self.crash_error().is_none() {
                return Ok(());
            }
            let _ = self.stop().await;
        }

        self.clear_crash_error();
        let options = options.unwrap_or_default();
        let mut child_command = crate::utils::subprocess_env::tokio_command(command);
        child_command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Maps to CC `subprocessEnv()` in `LSPClient.start(...)`: inherit the
        // parent environment by default, scrub CI/cloud secrets when the shared
        // subprocess scrub flag is set, then apply server-specific env overrides.
        crate::utils::subprocess_env::apply_subprocess_env(&mut child_command);
        if let Some(env) = options.env {
            child_command.envs(env);
        }
        if let Some(cwd) = options.cwd {
            child_command.current_dir(cwd);
        }

        let mut child = match child_command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.shared.start_failed.store(true, Ordering::SeqCst);
                *self.shared.start_error.lock().unwrap() = Some(error.to_string());
                return Err(error)
                    .with_context(|| format!("LSP server {} failed to start", self.server_name()));
            }
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("LSP server process stdout not available"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("LSP server process stdin not available"))?;
        let stderr = child.stderr.take();

        let (sender, receiver) = mpsc::channel::<Value>(64);
        let mut process = self.process.lock().await;
        process.writer_task = Some(tokio::spawn(writer_loop(
            self.shared.clone(),
            stdin,
            receiver,
        )));
        process.reader_task = Some(tokio::spawn(reader_loop(
            self.shared.clone(),
            stdout,
            sender.clone(),
        )));
        if let Some(stderr) = stderr {
            process.stderr_task = Some(tokio::spawn(stderr_loop(
                self.server_name().to_string(),
                stderr,
            )));
        }

        process.child = Some(child);
        drop(process);
        *self.shared.writer.lock().unwrap() = Some(sender);
        self.shared.start_failed.store(false, Ordering::SeqCst);
        *self.shared.start_error.lock().unwrap() = None;
        Ok(())
    }

    /// Maps to: CC `LSPClient.initialize(params)`.
    pub async fn initialize(&self, params: Value) -> anyhow::Result<Value> {
        if self.shared.writer.lock().unwrap().is_none() {
            anyhow::bail!("LSP client not started");
        }
        self.check_start_failed()?;

        let result = self.send_request_internal("initialize", params).await?;
        *self.shared.capabilities.lock().unwrap() = result.get("capabilities").cloned();
        self.send_notification("initialized", serde_json::json!({}))
            .await?;
        self.shared.is_initialized.store(true, Ordering::SeqCst);
        Ok(result)
    }

    /// Maps to: CC `LSPClient.sendRequest(method, params)`.
    pub async fn send_request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        if self.shared.writer.lock().unwrap().is_none() {
            anyhow::bail!("LSP client not started");
        }
        self.check_start_failed()?;
        if !self.shared.is_initialized.load(Ordering::SeqCst) {
            anyhow::bail!("LSP server not initialized");
        }
        self.send_request_internal(method, params).await
    }

    async fn send_request_internal(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let sender = self
            .shared
            .writer
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("LSP client not started"))?;
        // `vscode-jsonrpc` allocates the id inside the connection, so two
        // overlapping `sendRequest` calls never collide in CC either.
        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);

        let (response_sender, response_receiver) = oneshot::channel();
        self.shared
            .pending_requests
            .lock()
            .unwrap()
            .insert(id, response_sender);

        let message = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": method,
            "params": params,
        });

        if let Err(error) = sender.send(message).await {
            self.shared.pending_requests.lock().unwrap().remove(&id);
            anyhow::bail!(
                "LSP server {} connection closed before request {method} was sent: {error}",
                self.server_name()
            );
        }

        match response_receiver.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(anyhow::Error::new(error)),
            Err(_) => anyhow::bail!(
                "LSP server {} connection closed before response to {method}",
                self.server_name()
            ),
        }
    }

    /// Maps to: CC `LSPClient.sendNotification(method, params)`.
    pub async fn send_notification(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let sender = self.shared.writer.lock().unwrap().clone();
        let Some(sender) = sender else {
            anyhow::bail!("LSP client not started");
        };
        self.check_start_failed()?;

        let message = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "method": method,
            "params": params,
        });

        if let Err(error) = sender.send(message).await {
            tracing::debug!(
                server = %self.server_name(),
                method,
                error = %error,
                "LSP notification failed but continuing"
            );
        }
        Ok(())
    }

    /// Maps to: CC `LSPClient.onNotification(method, handler)`.
    pub fn on_notification<F>(&self, method: &str, handler: F)
    where
        F: Fn(Value) + Send + Sync + 'static,
    {
        self.shared
            .notification_handlers
            .lock()
            .unwrap()
            .entry(method.to_string())
            .or_default()
            .push(Arc::new(handler));
    }

    /// Maps to: CC `LSPClient.onRequest(method, handler)`.
    pub fn on_request<F>(&self, method: &str, handler: F)
    where
        F: Fn(Value) -> Value + Send + Sync + 'static,
    {
        self.shared
            .request_handlers
            .lock()
            .unwrap()
            .insert(method.to_string(), Arc::new(handler));
    }

    /// Maps to: CC `LSPClient.stop()`.
    ///
    /// CC awaits `connection.sendRequest('shutdown', {})` with no timeout
    /// (`LSPClient.ts:379-384`; `ast-grep -p 'Promise.race($$$)'` on that file
    /// matches nothing, and the only `withTimeout` in `services/lsp/` is
    /// `LSPServerInstance.ts:241`'s `startupTimeout`). The port previously
    /// capped it at 2s and raised an invented "stop timed out waiting for
    /// shutdown response" error; both are gone. The one addition kept is
    /// `child.wait()` — Rust must reap the child that Node's runtime reaps for
    /// it — and it is why the caller that must not block (start()'s cleanup)
    /// detaches instead, exactly as CC does at `LSPServerInstance.ts:256`.
    ///
    /// DECLARED DEVIATION: `tokio::process::Child::kill()` sends SIGKILL, where
    /// Node's `process.kill()` (`LSPClient.ts:418`) sends SIGTERM. Kept because
    /// it arrives strictly AFTER the graceful `shutdown` + `exit` handshake CC
    /// also performs, so it only affects a server that ignored both; making it
    /// SIGTERM would need a `libc`/`nix` signal call this crate does not
    /// otherwise take a dependency on.
    pub async fn stop(&self) -> anyhow::Result<()> {
        let mut shutdown_error: Option<anyhow::Error> = None;
        self.shared.is_stopping.store(true, Ordering::SeqCst);

        if self.shared.writer.lock().unwrap().is_some() {
            match self
                .send_request_internal("shutdown", serde_json::json!({}))
                .await
            {
                Ok(_) => {}
                Err(error) => shutdown_error = Some(error),
            }
            let _ = self.send_notification("exit", serde_json::json!({})).await;
            tokio::task::yield_now().await;
        }

        self.shared.writer.lock().unwrap().take();
        {
            let mut process = self.process.lock().await;
            if let Some(task) = process.reader_task.take() {
                task.abort();
            }
            if let Some(task) = process.writer_task.take() {
                task.abort();
            }
            if let Some(task) = process.stderr_task.take() {
                task.abort();
            }
            if let Some(mut child) = process.child.take() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }

        drain_pending_requests(
            &self.shared.pending_requests,
            JsonRpcResponseError {
                code: None,
                message: format!("LSP server {} stopped", self.server_name()),
                data: None,
            },
        );

        self.shared.is_initialized.store(false, Ordering::SeqCst);
        *self.shared.capabilities.lock().unwrap() = None;
        self.clear_crash_error();
        self.shared.is_stopping.store(false, Ordering::SeqCst);
        if let Some(error) = shutdown_error {
            self.shared.start_failed.store(true, Ordering::SeqCst);
            *self.shared.start_error.lock().unwrap() = Some(error.to_string());
            return Err(error);
        }
        Ok(())
    }
}

async fn writer_loop(
    shared: Arc<LspClientShared>,
    mut stdin: tokio::process::ChildStdin,
    mut receiver: mpsc::Receiver<Value>,
) {
    let server_name = shared.server_name.clone();
    while let Some(message) = receiver.recv().await {
        let frame = match encode_lsp_message(&message) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::debug!(server = %server_name, error = %error, "failed to encode LSP message");
                continue;
            }
        };
        if let Err(error) = stdin.write_all(&frame).await {
            tracing::debug!(server = %server_name, error = %error, "LSP stdin write failed");
            let message = format!("LSP server {server_name} stdin write failed: {error}");
            record_transport_crash(&shared, message.clone());
            drain_pending_requests(
                &shared.pending_requests,
                JsonRpcResponseError {
                    code: None,
                    message,
                    data: None,
                },
            );
            break;
        }
        if let Err(error) = stdin.flush().await {
            tracing::debug!(server = %server_name, error = %error, "LSP stdin flush failed");
            let message = format!("LSP server {server_name} stdin flush failed: {error}");
            record_transport_crash(&shared, message.clone());
            drain_pending_requests(
                &shared.pending_requests,
                JsonRpcResponseError {
                    code: None,
                    message,
                    data: None,
                },
            );
            break;
        }
    }
}

async fn reader_loop(
    shared: Arc<LspClientShared>,
    mut stdout: tokio::process::ChildStdout,
    response_sender: mpsc::Sender<Value>,
) {
    let server_name = shared.server_name.clone();
    loop {
        match read_lsp_message(&mut stdout).await {
            Ok(Some(message)) => {
                handle_incoming_message(&shared, message, &response_sender).await;
            }
            Ok(None) => {
                let message = format!("LSP server {server_name} connection closed");
                record_transport_crash(&shared, message.clone());
                drain_pending_requests(
                    &shared.pending_requests,
                    JsonRpcResponseError {
                        code: None,
                        message,
                        data: None,
                    },
                );
                break;
            }
            Err(error) => {
                tracing::debug!(server = %server_name, error = %error, "LSP stdout read failed");
                let message = error.to_string();
                record_transport_crash(&shared, message.clone());
                drain_pending_requests(
                    &shared.pending_requests,
                    JsonRpcResponseError {
                        code: None,
                        message,
                        data: None,
                    },
                );
                break;
            }
        }
    }
}

async fn stderr_loop(server_name: String, mut stderr: tokio::process::ChildStderr) {
    let mut buf = vec![0; 1024];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let output = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                if !output.is_empty() {
                    tracing::debug!(server = %server_name, output, "LSP server stderr");
                }
            }
            Err(error) => {
                tracing::debug!(server = %server_name, error = %error, "LSP stderr read failed");
                break;
            }
        }
    }
}

async fn handle_incoming_message(
    shared: &Arc<LspClientShared>,
    message: Value,
    response_sender: &mpsc::Sender<Value>,
) {
    let server_name = &shared.server_name;
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = message.get("id").cloned();

    match (method, id) {
        (Some(method), Some(id)) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let handler = shared
                .request_handlers
                .lock()
                .unwrap()
                .get(&method)
                .cloned();
            let response = if let Some(handler) = handler {
                let result = handler(params);
                serde_json::json!({
                    "jsonrpc": JSONRPC_VERSION,
                    "id": id,
                    "result": result,
                })
            } else {
                serde_json::json!({
                    "jsonrpc": JSONRPC_VERSION,
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {method}"),
                    },
                })
            };
            let _ = response_sender.send(response).await;
        }
        (Some(method), None) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let handlers = shared
                .notification_handlers
                .lock()
                .unwrap()
                .get(&method)
                .cloned()
                .unwrap_or_default();
            if handlers.is_empty() {
                tracing::debug!(server = %server_name, method, "unhandled LSP notification");
            }
            for handler in handlers {
                handler(params.clone());
            }
        }
        (None, Some(id)) => {
            let Some(request_id) = json_rpc_id_to_i64(&id) else {
                tracing::debug!(server = %server_name, id = %id, "received LSP response with unsupported id");
                return;
            };
            let response_sender = shared.pending_requests.lock().unwrap().remove(&request_id);
            if let Some(sender) = response_sender {
                let result = if let Some(error) = message.get("error") {
                    Err(JsonRpcResponseError {
                        code: error.get("code").and_then(Value::as_i64),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("LSP request failed")
                            .to_string(),
                        data: error.get("data").cloned(),
                    })
                } else {
                    Ok(message.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = sender.send(result);
            }
        }
        (None, None) => {
            tracing::debug!(server = %server_name, "received LSP message without method or id");
        }
    }
}

/// Maps to: CC `LSPClient.ts:156-167` `process.on('exit', ...)`.
///
/// CC ignores the event while an intentional stop is running (`!isStopping`),
/// clears `startFailed`/`startError`, drops `isInitialized`, and calls
/// `onCrash` so the owner flips to `'error'` AT CRASH TIME — that callback is
/// why `ensureServerStarted` restarts a dead server transparently instead of
/// costing one failed tool call first.
fn record_transport_crash(shared: &Arc<LspClientShared>, message: String) {
    if shared.is_stopping.load(Ordering::SeqCst) {
        return;
    }
    {
        let mut slot = shared.crash_error.lock().unwrap();
        if slot.is_some() {
            return;
        }
        *slot = Some(message.clone());
    }
    shared.is_initialized.store(false, Ordering::SeqCst);
    shared.start_failed.store(false, Ordering::SeqCst);
    *shared.start_error.lock().unwrap() = None;
    if let Some(handler) = shared.on_crash.as_ref() {
        handler(message);
    }
}

fn drain_pending_requests(pending_requests: &PendingRequests, error: JsonRpcResponseError) {
    let pending = std::mem::take(&mut *pending_requests.lock().unwrap());
    for (_, sender) in pending {
        let _ = sender.send(Err(error.clone()));
    }
}

fn json_rpc_id_to_i64(id: &Value) -> Option<i64> {
    match id {
        Value::Number(number) => number.as_i64(),
        Value::String(value) => value.parse::<i64>().ok(),
        _ => None,
    }
}

fn encode_lsp_message(message: &Value) -> anyhow::Result<Vec<u8>> {
    let body = serde_json::to_vec(message)?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    Ok(frame)
}

async fn read_lsp_message<R>(reader: &mut R) -> anyhow::Result<Option<Value>>
where
    R: AsyncRead + Unpin,
{
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        let read = reader.read(&mut byte).await?;
        if read == 0 {
            if header.is_empty() {
                return Ok(None);
            }
            anyhow::bail!("LSP stream ended while reading headers");
        }
        header.push(byte[0]);
        if header.len() > MAX_HEADER_BYTES {
            anyhow::bail!("LSP header exceeded {MAX_HEADER_BYTES} bytes");
        }
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }

    let header_text = std::str::from_utf8(&header).context("LSP header was not UTF-8")?;
    let content_length = parse_content_length(header_text)
        .ok_or_else(|| anyhow!("LSP message missing Content-Length header"))?;
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body).await?;
    let message = serde_json::from_slice(&body).context("LSP message body was not valid JSON")?;
    Ok(Some(message))
}

fn parse_content_length(header_text: &str) -> Option<usize> {
    header_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("Content-Length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn encode_message_uses_lsp_content_length_frame() {
        let frame =
            encode_lsp_message(&serde_json::json!({"jsonrpc":"2.0","id":1,"result":null})).unwrap();
        let text = String::from_utf8(frame.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "));
        assert!(text.contains("\r\n\r\n"));
        let (_, body) = text.split_once("\r\n\r\n").unwrap();
        assert_eq!(
            parse_content_length(text.split("\r\n\r\n").next().unwrap()).unwrap(),
            body.as_bytes().len()
        );
    }

    #[tokio::test]
    async fn read_lsp_message_decodes_content_length_frame() {
        let frame =
            encode_lsp_message(&serde_json::json!({"jsonrpc":"2.0","id":7,"result":{"ok":true}}))
                .unwrap();
        let (mut client, mut server) = tokio::io::duplex(1024);
        server.write_all(&frame).await.unwrap();
        server.shutdown().await.unwrap();

        let message = read_lsp_message(&mut client).await.unwrap().unwrap();
        assert_eq!(message["id"], 7);
        assert_eq!(message["result"]["ok"], true);
    }

    #[tokio::test]
    async fn lsp_client_initializes_and_sends_request_over_stdio() {
        if crate::utils::subprocess_env::command("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let script = write_fake_lsp_server();
        let client = create_lsp_client("fake", None);
        client
            .start(
                "node",
                &[script.display().to_string()],
                Some(LspClientStartOptions::default()),
            )
            .await
            .unwrap();

        let init = client
            .initialize(serde_json::json!({"capabilities":{},"processId":null}))
            .await
            .unwrap();
        assert_eq!(init["capabilities"]["hoverProvider"], true);
        assert_eq!(client.capabilities().unwrap()["hoverProvider"], true);
        assert!(client.is_initialized());

        let hover = client
            .send_request(
                "textDocument/hover",
                serde_json::json!({"textDocument":{"uri":"file:///tmp/a.rs"}}),
            )
            .await
            .unwrap();
        assert_eq!(hover["contents"], "hello from fake lsp");

        client.stop().await.unwrap();
        let _ = fs::remove_file(script);
    }

    fn write_fake_lsp_server() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cometix-fake-lsp-{unique}.mjs"));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(
            br#"
let buffer = Buffer.alloc(0);
function send(message) {
  const body = Buffer.from(JSON.stringify(message));
  process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`);
  process.stdout.write(body);
}
function handle(message) {
  if (message.method === 'initialize') {
    send({ jsonrpc: '2.0', id: message.id, result: { capabilities: { hoverProvider: true } } });
  } else if (message.method === 'initialized') {
    // Notification, no response.
  } else if (message.method === 'textDocument/hover') {
    send({ jsonrpc: '2.0', id: message.id, result: { contents: 'hello from fake lsp' } });
  } else if (message.method === 'shutdown') {
    send({ jsonrpc: '2.0', id: message.id, result: null });
  } else if (message.method === 'exit') {
    process.exit(0);
  } else if (message.id !== undefined) {
    send({ jsonrpc: '2.0', id: message.id, result: null });
  }
}
process.stdin.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  while (true) {
    const headerEnd = buffer.indexOf('\r\n\r\n');
    if (headerEnd === -1) return;
    const header = buffer.slice(0, headerEnd).toString();
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) throw new Error('missing Content-Length');
    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    if (buffer.length < bodyStart + length) return;
    const body = buffer.slice(bodyStart, bodyStart + length);
    buffer = buffer.slice(bodyStart + length);
    handle(JSON.parse(body.toString()));
  }
});
"#,
        )
        .unwrap();
        path
    }
}
