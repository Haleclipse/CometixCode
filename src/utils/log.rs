//! Maps to: CC `utils/log.ts` (error-event frontend only).
//! Log/session display, loading, and API capture remain with their existing
//! owners or unported; this file does not claim those independent functions.

use crate::utils::process_env::JsTruthy;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::{
    io,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
};

/// L1 native carrier for JavaScript's built-in Error, not a new CC error class.
/// The sink consumes the same name/message/optional stack and Axios fields.
#[derive(Clone, Debug, PartialEq)]
pub struct LogError {
    pub name: String,
    pub message: String,
    pub stack: Option<String>,
    pub axios: Option<AxiosErrorContext>,
}

impl LogError {
    pub fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        let header = if message.is_empty() {
            "Error".to_owned()
        } else {
            format!("Error: {message}")
        };
        let stack = Some(format!(
            "{header}\n{}",
            std::backtrace::Backtrace::force_capture()
        ));
        Self {
            name: "Error".into(),
            message,
            stack,
            axios: None,
        }
    }

    /// JavaScript `error.stack || error.message` (an empty stack falls back).
    pub fn stack_or_message(&self) -> &str {
        self.stack
            .as_deref()
            .filter(|stack| !stack.is_empty())
            .unwrap_or(&self.message)
    }
}

/// L1 carrier for `axios.isAxiosError` and the request/response fields read by
/// CC `utils/errorLogSink.ts:156-171`; no HTTP request is performed here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AxiosErrorContext {
    pub url: Option<String>,
    pub status: Option<u16>,
    pub data: Option<Value>,
}

/// L1 `unknown` carrier: preserve `instanceof Error` versus plain JSON values.
/// Undefined is distinct from JSON null, including in String conversion.
#[derive(Clone, Debug, PartialEq)]
pub enum McpLogError {
    Error(LogError),
    Value(Value),
    Undefined,
}

impl std::fmt::Display for McpLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error(error) => {
                // ECMAScript Error.prototype.toString: no stack in templates.
                if error.name.is_empty() {
                    f.write_str(&error.message)
                } else if error.message.is_empty() {
                    f.write_str(&error.name)
                } else {
                    write!(f, "{}: {}", error.name, error.message)
                }
            }
            Self::Value(value) => f.write_str(&crate::utils::zod::js_string(value)),
            Self::Undefined => f.write_str("undefined"),
        }
    }
}

impl From<LogError> for McpLogError {
    fn from(error: LogError) -> Self {
        Self::Error(error)
    }
}
impl From<Value> for McpLogError {
    fn from(value: Value) -> Self {
        Self::Value(value)
    }
}

/// Maps to: CC `utils/log.ts:60-62` `dateToFilename`.
pub fn date_to_filename(date: DateTime<Utc>) -> String {
    date.to_rfc3339_opts(SecondsFormat::Millis, true)
        .replace([':', '.'], "-")
}

/// Maps to: CC `utils/log.ts:66-67` in-memory error records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InMemoryError {
    pub error: String,
    pub timestamp: String,
}
const MAX_IN_MEMORY_ERRORS: usize = 100;

/// Maps to: CC `utils/log.ts:82-88` `ErrorLogSink`.
/// L1 callbacks use Result for source throws; callers retain each catch boundary.
/// Native panics are not JS thrown-value substitutes and are not intercepted.
#[derive(Clone)]
pub struct ErrorLogSink {
    pub log_error: Arc<dyn Fn(LogError) -> io::Result<()> + Send + Sync>,
    pub log_mcp_error: Arc<dyn Fn(&str, McpLogError) -> io::Result<()> + Send + Sync>,
    pub log_mcp_debug: Arc<dyn Fn(&str, &str) -> io::Result<()> + Send + Sync>,
    pub get_errors_path: Arc<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
    pub get_mcp_logs_path: Arc<dyn Fn(&str) -> io::Result<PathBuf> + Send + Sync>,
}

/// Maps to: CC `utils/log.ts:91-94` `QueuedErrorEvent`.
enum QueuedErrorEvent {
    Error(LogError),
    McpError {
        server_name: String,
        error: McpLogError,
    },
    McpDebug {
        server_name: String,
        message: String,
    },
}

/// L1 shared-state carrier for the three module variables at log.ts:67,96,99.
/// Callbacks execute outside the mutex so a sink can synchronously log again.
#[derive(Default)]
struct ErrorLogState {
    in_memory_error_log: Vec<InMemoryError>,
    error_queue: Vec<QueuedErrorEvent>,
    error_log_sink: Option<ErrorLogSink>,
}
static ERROR_LOG_STATE: LazyLock<Mutex<ErrorLogState>> =
    LazyLock::new(|| Mutex::new(ErrorLogState::default()));

// Native carrier for source synchronous log.ts turns. The state mutex remains
// short-lived; this separate entry guard prevents other OS threads overtaking
// the attach/drain turn while permitting the source's synchronous callback
// reentry on the same thread. It adds no logging queue or ordering policy.
static LOG_TURN: Mutex<()> = Mutex::new(());
thread_local! {
    static IN_LOG_TURN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
struct LogTurn {
    guard: Option<std::sync::MutexGuard<'static, ()>>,
}
impl LogTurn {
    fn enter() -> Self {
        if IN_LOG_TURN.with(std::cell::Cell::get) {
            return Self { guard: None };
        }
        let guard = LOG_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        IN_LOG_TURN.with(|entered| entered.set(true));
        Self { guard: Some(guard) }
    }
}
impl Drop for LogTurn {
    fn drop(&mut self) {
        if self.guard.is_some() {
            IN_LOG_TURN.with(|entered| entered.set(false));
        }
    }
}

/// Maps to: CC `utils/log.ts:69-77` `addToInMemoryErrorLog`.
/// The argument is the already-locked module variable, not a second log owner.
fn add_to_in_memory_error_log(log: &mut Vec<InMemoryError>, error_info: InMemoryError) {
    if log.len() >= MAX_IN_MEMORY_ERRORS {
        log.remove(0);
    }
    log.push(error_info);
}

/// Maps to: CC `utils/log.ts:109-134` `attachErrorLogSink`.
/// Installation precedes draining a snapshot; the first callback error escapes
/// and discards the remainder, exactly as the source's uncaught throw does.
pub fn attach_error_log_sink(new_sink: ErrorLogSink) -> io::Result<()> {
    let _turn = LogTurn::enter();
    let queued_events = {
        let mut state = ERROR_LOG_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.error_log_sink.is_some() {
            return Ok(());
        }
        state.error_log_sink = Some(new_sink.clone());
        std::mem::take(&mut state.error_queue)
    };
    for event in queued_events {
        match event {
            QueuedErrorEvent::Error(error) => (new_sink.log_error)(error)?,
            QueuedErrorEvent::McpError { server_name, error } => {
                (new_sink.log_mcp_error)(&server_name, error)?
            }
            QueuedErrorEvent::McpDebug {
                server_name,
                message,
            } => (new_sink.log_mcp_debug)(&server_name, &message)?,
        }
    }
    Ok(())
}

/// Maps to: CC `utils/log.ts:158-199` `logError`.
/// Partial build boundary: source HARD_FAIL is absent from the current Rust
/// build-feature matrix; no independent runtime switch is invented here.
pub fn log_error(error: impl Into<McpLogError>) {
    let _turn = LogTurn::enter();
    let err = crate::utils::errors::to_error(error.into());
    if [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ]
    .iter()
    .any(|key| crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var(key).as_deref()))
        || crate::utils::process_env::var("DISABLE_ERROR_REPORTING")
            .truthy()
            .is_some()
        || crate::utils::privacy_level::is_essential_traffic_only()
    {
        return;
    }
    let sink = {
        let mut state = ERROR_LOG_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        add_to_in_memory_error_log(
            &mut state.in_memory_error_log,
            InMemoryError {
                error: err.stack_or_message().to_owned(),
                timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            },
        );
        let Some(sink) = state.error_log_sink.clone() else {
            state.error_queue.push(QueuedErrorEvent::Error(err));
            return;
        };
        sink
    };
    let _ = (sink.log_error)(err);
}

/// Maps to: CC `utils/log.ts:201-203` `getInMemoryErrors`.
pub fn get_in_memory_errors() -> Vec<InMemoryError> {
    let _turn = LogTurn::enter();
    ERROR_LOG_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .in_memory_error_log
        .clone()
}

/// Maps to: CC `utils/log.ts:300-312` `logMCPError`.
/// Unlike logError, MCP events do not use the reporting/privacy gate.
pub fn log_mcp_error(server_name: &str, error: impl Into<McpLogError>) {
    let _turn = LogTurn::enter();
    let error = error.into();
    let sink = {
        let mut state = ERROR_LOG_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(sink) = state.error_log_sink.clone() else {
            state.error_queue.push(QueuedErrorEvent::McpError {
                server_name: server_name.into(),
                error,
            });
            return;
        };
        sink
    };
    let _ = (sink.log_mcp_error)(server_name, error);
}

/// Maps to: CC `utils/log.ts:314-326` `logMCPDebug`.
pub fn log_mcp_debug(server_name: &str, message: &str) {
    let _turn = LogTurn::enter();
    let sink = {
        let mut state = ERROR_LOG_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(sink) = state.error_log_sink.clone() else {
            state.error_queue.push(QueuedErrorEvent::McpDebug {
                server_name: server_name.into(),
                message: message.into(),
            });
            return;
        };
        sink
    };
    let _ = (sink.log_mcp_debug)(server_name, message);
}

/// Maps to: CC `utils/log.ts:358-362` `_resetErrorLogForTesting`.
#[cfg(test)]
pub fn _reset_error_log_for_testing() {
    let _turn = LogTurn::enter();
    *ERROR_LOG_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = ErrorLogState::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;

    fn clean_env() -> Vec<EnvVarGuard> {
        [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "DISABLE_ERROR_REPORTING",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ]
        .into_iter()
        .map(EnvVarGuard::unset)
        .collect()
    }
    fn record_sink(events: Arc<Mutex<Vec<String>>>) -> ErrorLogSink {
        let errors = events.clone();
        let mcp_errors = events.clone();
        ErrorLogSink {
            log_error: Arc::new(move |error| {
                errors
                    .lock()
                    .unwrap()
                    .push(format!("error:{}", error.message));
                Ok(())
            }),
            log_mcp_error: Arc::new(move |server, error| {
                mcp_errors
                    .lock()
                    .unwrap()
                    .push(format!("mcp:{server}:{error}"));
                Ok(())
            }),
            log_mcp_debug: Arc::new(move |server, message| {
                events
                    .lock()
                    .unwrap()
                    .push(format!("debug:{server}:{message}"));
                Ok(())
            }),
            get_errors_path: Arc::new(|| Ok(PathBuf::new())),
            get_mcp_logs_path: Arc::new(|_| Ok(PathBuf::new())),
        }
    }

    #[test]
    fn date_to_filename_matches_official_utc_milliseconds() {
        let date = DateTime::parse_from_rfc3339("2026-09-11T08:09:10.123456+08:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(date_to_filename(date), "2026-09-11T00-09-10-123Z");
    }

    #[test]
    fn queued_events_match_official_order_idempotence_and_reentry() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = clean_env();
        _reset_error_log_for_testing();
        log_error(LogError::new("first"));
        log_mcp_error("srv", json!({"a": 1}));
        log_mcp_debug("srv", "last");
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut sink = record_sink(events.clone());
        let errors = events.clone();
        sink.log_error = Arc::new(move |error| {
            errors
                .lock()
                .unwrap()
                .push(format!("error:{}", error.message));
            log_mcp_debug("nested", "during-drain");
            Ok(())
        });
        attach_error_log_sink(sink).unwrap();
        let ignored = Arc::new(Mutex::new(Vec::new()));
        attach_error_log_sink(record_sink(ignored.clone())).unwrap();
        log_mcp_debug("srv", "after");
        assert_eq!(
            *events.lock().unwrap(),
            [
                "error:first",
                "debug:nested:during-drain",
                "mcp:srv:[object Object]",
                "debug:srv:last",
                "debug:srv:after"
            ]
        );
        assert!(ignored.lock().unwrap().is_empty());
        _reset_error_log_for_testing();
    }

    #[test]
    fn attach_drain_matches_official_synchronous_turn_against_external_threads() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        _reset_error_log_for_testing();
        log_mcp_debug("srv", "A");
        log_mcp_debug("srv", "B");
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let mut sink = record_sink(events.clone());
        let observed = events.clone();
        sink.log_mcp_debug = Arc::new(move |_, message| {
            observed.lock().unwrap().push(message.to_owned());
            if message == "A" {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                log_mcp_debug("srv", "nested");
            }
            Ok(())
        });
        let installing = std::thread::spawn(move || attach_error_log_sink(sink));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let external = std::thread::spawn(move || {
            attempt_tx.send(()).unwrap();
            log_mcp_debug("srv", "C");
            done_tx.send(()).unwrap();
        });
        attempt_rx.recv().unwrap();
        let premature = done_rx.recv_timeout(std::time::Duration::from_millis(50));
        release_tx.send(()).unwrap();
        installing.join().unwrap().unwrap();
        external.join().unwrap();
        assert!(
            premature.is_err(),
            "an independent event cannot interrupt the source synchronous drain"
        );
        assert_eq!(*events.lock().unwrap(), ["A", "nested", "B", "C"]);
        _reset_error_log_for_testing();
    }

    #[test]
    fn error_gate_and_ring_match_official_but_mcp_still_logs() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = clean_env();
        _reset_error_log_for_testing();
        let events = Arc::new(Mutex::new(Vec::new()));
        attach_error_log_sink(record_sink(events.clone())).unwrap();
        for key in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "DISABLE_ERROR_REPORTING",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ] {
            let value = if key.starts_with("CLAUDE_CODE_USE_") {
                "true"
            } else {
                "0"
            };
            let _env = EnvVarGuard::set(key, value);
            log_error(json!("hidden"));
            log_mcp_debug("srv", key);
            log_mcp_error("srv", Value::Null);
        }
        assert!(get_in_memory_errors().is_empty());
        assert_eq!(events.lock().unwrap().len(), 10);
        for index in 0..101 {
            log_error(LogError {
                name: "Error".into(),
                message: format!("e{index}"),
                stack: None,
                axios: None,
            });
        }
        let mut snapshot = get_in_memory_errors();
        assert_eq!(snapshot.len(), 100);
        assert_eq!(snapshot[0].error, "e1");
        assert_eq!(snapshot[99].error, "e100");
        assert!(DateTime::parse_from_rfc3339(&snapshot[0].timestamp).is_ok());
        snapshot.clear();
        assert_eq!(get_in_memory_errors().len(), 100);
        _reset_error_log_for_testing();
    }

    #[test]
    fn drain_failures_match_official_throw_but_direct_calls_swallow() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = clean_env();
        _reset_error_log_for_testing();
        log_mcp_debug("srv", "first");
        log_mcp_debug("srv", "discarded");
        let calls = Arc::new(Mutex::new(0));
        let seen = calls.clone();
        let mut sink = record_sink(Arc::new(Mutex::new(Vec::new())));
        sink.log_mcp_debug = Arc::new(move |_, _| {
            *seen.lock().unwrap() += 1;
            Err(io::Error::other("write failed"))
        });
        assert_eq!(
            attach_error_log_sink(sink).unwrap_err().to_string(),
            "write failed"
        );
        log_mcp_debug("srv", "direct");
        assert_eq!(*calls.lock().unwrap(), 2);
        attach_error_log_sink(record_sink(Arc::new(Mutex::new(Vec::new())))).unwrap();
        assert_eq!(*calls.lock().unwrap(), 2);
        _reset_error_log_for_testing();
    }

    #[test]
    fn unknown_error_strings_match_official_error_identity_and_js_coercion() {
        assert_eq!(McpLogError::Undefined.to_string(), "undefined");
        assert_eq!(
            McpLogError::Value(json!([null, 1, {"a":2}, [3,4]])).to_string(),
            ",1,[object Object],3,4"
        );
        let mut error = LogError {
            name: "TypeError".into(),
            message: "bad".into(),
            stack: Some("native stack".into()),
            axios: None,
        };
        assert_eq!(
            McpLogError::Error(error.clone()).to_string(),
            "TypeError: bad"
        );
        assert_eq!(error.stack_or_message(), "native stack");
        error.stack = Some(String::new());
        assert_eq!(error.stack_or_message(), "bad");
        error.name.clear();
        assert_eq!(McpLogError::Error(error.clone()).to_string(), "bad");
        error.message.clear();
        assert_eq!(McpLogError::Error(error).to_string(), "");
    }
}
