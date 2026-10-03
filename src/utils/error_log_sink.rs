//! Maps to: CC `utils/errorLogSink.ts`.
//! File-backed error and MCP logging; the lightweight queue stays in `log.rs`.

use crate::utils::buffered_writer::{
    BufferedWriter, BufferedWriterOptions, create_buffered_writer,
};
use crate::utils::cache_paths::CACHE_PATHS;
use crate::utils::debug::{DebugLogLevel, log_for_debugging, log_for_debugging_with_level};
use crate::utils::log::{
    ErrorLogSink, LogError, McpLogError, attach_error_log_sink, date_to_filename,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

// Maps to: CC utils/errorLogSink.ts:24 module DATE. Forced when this backend
// initializes, preserving one filename timestamp for its entire lifetime.
static DATE: LazyLock<String> = LazyLock::new(|| date_to_filename(chrono::Utc::now()));

/// Maps to: CC `utils/errorLogSink.ts:29-31` `getErrorsPath`.
pub fn get_errors_path() -> io::Result<PathBuf> {
    Ok(CACHE_PATHS.errors()?.join(format!("{}.jsonl", *DATE)))
}

/// Maps to: CC `utils/errorLogSink.ts:36-38` `getMCPLogsPath`.
pub fn get_mcp_logs_path(server_name: &str) -> io::Result<PathBuf> {
    Ok(CACHE_PATHS
        .mcp_logs(server_name)?
        .join(format!("{}.jsonl", *DATE)))
}

/// Maps to: CC `utils/errorLogSink.ts:40-44` `JsonlWriter`.
#[derive(Clone)]
struct JsonlWriter {
    writer: BufferedWriter,
}

impl JsonlWriter {
    /// Maps to: CC `utils/errorLogSink.ts:53-55` `createJsonlWriter.write`.
    fn write(&self, value: &Value) -> io::Result<()> {
        self.writer
            .write(&format!("{}\n", serde_json::to_string(value)?))
    }

    /// Maps to: CC `utils/errorLogSink.ts:56` delegated `flush`.
    fn flush(&self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Maps to: CC `utils/errorLogSink.ts:57` delegated `dispose`.
    fn dispose(&self) -> io::Result<()> {
        self.writer.dispose()
    }
}

/// Maps to: CC `utils/errorLogSink.ts:46-59` `createJsonlWriter`.
fn create_jsonl_writer(options: BufferedWriterOptions) -> io::Result<JsonlWriter> {
    Ok(JsonlWriter {
        writer: create_buffered_writer(options)?,
    })
}

// Maps to: CC utils/errorLogSink.ts:62 logWriters, keyed by complete path.
static LOG_WRITERS: LazyLock<Mutex<HashMap<PathBuf, JsonlWriter>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maps to: CC `utils/errorLogSink.ts:85-109` `getLogWriter`.
fn get_log_writer(path: &Path) -> io::Result<JsonlWriter> {
    let mut writers = LOG_WRITERS.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(writer) = writers.get(path) {
        return Ok(writer.clone());
    }
    let path_for_write = path.to_owned();
    let mut options = BufferedWriterOptions::new(Arc::new(move |content: &str| {
        // Maps to: CC :92-100 append, then mkdir and retry on any append error.
        // Native fs calls carry NodeFsOperations :468-486/:528-537; its full
        // injectable filesystem interface remains outside this source slice.
        let append = || {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path_for_write)?
                .write_all(content.as_bytes())
        };
        if append().is_err() {
            std::fs::create_dir_all(path_for_write.parent().unwrap_or(Path::new(".")))?;
            append()?;
        }
        Ok(())
    }));
    options.flush_interval_ms = 1000;
    options.max_buffer_size = 50;
    let writer = create_jsonl_writer(options)?;
    writers.insert(path.to_owned(), writer.clone());
    let cleanup_writer = writer.clone();
    crate::utils::cleanup_registry::register_cleanup(move || {
        let writer = cleanup_writer.clone();
        async move {
            // Maps to cleanupRegistry.ts:18-19 and gracefulShutdown.ts:445-450:
            // source shutdown silently ignores rejected cleanup promises. The
            // existing () cleanup carrier discards that Result at this boundary.
            let _ = writer.dispose();
        }
    });
    Ok(writer)
}

/// Maps to: CC `utils/errorLogSink.ts:111-126` `appendToLog`.
/// L1 Compile-time distribution capability projection: the source ant gate
/// and its userType field come from the immutable build audience.
fn append_to_log(path: &Path, message: &Value) -> io::Result<()> {
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::TelemetryPayloads,
    ) {
        return Ok(());
    }
    let mut value = json!({"timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)});
    if let Some(fields) = message.as_object() {
        for (key, field) in fields {
            value[key] = field.clone();
        }
    }
    value["cwd"] = json!(std::env::current_dir()?.to_string_lossy());
    value["userType"] = json!("ant");
    value["sessionId"] = json!(crate::bootstrap::state::get_session_id());
    value["version"] = json!(crate::constants::product::VERSION);
    get_log_writer(path)?.write(&value)
}

/// Maps to: CC `utils/errorLogSink.ts:128-147` `extractServerMessage`.
fn extract_server_message(data: &Value) -> Option<&str> {
    data.as_str()
        .or_else(|| data.get("message").and_then(Value::as_str))
        .or_else(|| {
            data.get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
}

/// Maps to: CC `utils/errorLogSink.ts:152-174` `logErrorImpl`.
fn log_error_impl(error: LogError) -> io::Result<()> {
    let error_str = error
        .stack
        .as_deref()
        .filter(|stack| !stack.is_empty())
        .unwrap_or(&error.message);
    let mut context = String::new();
    if let Some(axios) = &error.axios {
        if let Some(url) = axios.url.as_deref().filter(|url| !url.is_empty()) {
            let mut parts = vec![format!("url={url}")];
            if let Some(status) = axios.status {
                parts.push(format!("status={status}"));
            }
            if let Some(message) = axios
                .data
                .as_ref()
                .and_then(extract_server_message)
                .filter(|message| !message.is_empty())
            {
                parts.push(format!("body={message}"));
            }
            context = format!("[{}] ", parts.join(","));
        }
    }
    log_for_debugging_with_level(
        &format!("{}: {context}{error_str}", error.name),
        DebugLogLevel::Error,
    );
    append_to_log(
        &get_errors_path()?,
        &json!({"error": format!("{context}{error_str}")}),
    )
}

/// Maps to: CC `utils/errorLogSink.ts:179-195` `logMCPErrorImpl`.
fn log_mcp_error_impl(server_name: &str, error: McpLogError) -> io::Result<()> {
    log_for_debugging_with_level(
        &format!("MCP server \"{server_name}\" {}", error.to_string()),
        DebugLogLevel::Error,
    );
    let path = get_mcp_logs_path(server_name)?;
    let error_str = match &error {
        McpLogError::Error(error) => error
            .stack
            .as_deref()
            .filter(|stack| !stack.is_empty())
            .unwrap_or(&error.message)
            .to_owned(),
        _ => error.to_string(),
    };
    get_log_writer(&path)?.write(&json!({
        "error": error_str,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "sessionId": crate::bootstrap::state::get_session_id(),
        "cwd": std::env::current_dir()?.to_string_lossy(),
    }))
}

/// Maps to: CC `utils/errorLogSink.ts:200-213` `logMCPDebugImpl`.
fn log_mcp_debug_impl(server_name: &str, message: &str) -> io::Result<()> {
    log_for_debugging(&format!("MCP server \"{server_name}\": {message}"));
    let path = get_mcp_logs_path(server_name)?;
    get_log_writer(&path)?.write(&json!({
        "debug": message,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "sessionId": crate::bootstrap::state::get_session_id(),
        "cwd": std::env::current_dir()?.to_string_lossy(),
    }))
}

/// Maps to: CC `utils/errorLogSink.ts:225-235` `initializeErrorLogSink`.
pub fn initialize_error_log_sink() -> io::Result<()> {
    LazyLock::force(&DATE);
    attach_error_log_sink(ErrorLogSink {
        log_error: Arc::new(log_error_impl),
        log_mcp_error: Arc::new(log_mcp_error_impl),
        log_mcp_debug: Arc::new(log_mcp_debug_impl),
        get_errors_path: Arc::new(get_errors_path),
        get_mcp_logs_path: Arc::new(get_mcp_logs_path),
    })?;
    log_for_debugging("Error log sink initialized");
    Ok(())
}

/// Maps to: CC `utils/errorLogSink.ts:68-72` `_flushLogWritersForTesting`.
#[cfg(test)]
pub(crate) fn _flush_log_writers_for_testing() -> io::Result<()> {
    let writers: Vec<_> = LOG_WRITERS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    for writer in writers {
        writer.flush()?;
    }
    Ok(())
}

/// Maps to: CC `utils/errorLogSink.ts:78-83` `_clearLogWritersForTesting`.
#[cfg(test)]
pub(crate) fn _clear_log_writers_for_testing() -> io::Result<()> {
    let mut writers = LOG_WRITERS.lock().unwrap_or_else(|p| p.into_inner());
    for writer in writers.values() {
        writer.dispose()?;
    }
    writers.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::log::{self, AxiosErrorContext};
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("cometix-mcp-log-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn read_records(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    // CC errorLogSink.ts:200-213, log.ts:314-324, messages.ts:3933-3936.
    // Drives the actual pre-start queue -> startup -> normalization -> shutdown
    // -> JSONL chain with no model request or substitute writer.
    #[test]
    fn mcp_resource_log_matches_official_startup_queue_and_shutdown_persistence() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let home = TempDir::new();
        let _home = EnvVarGuard::set(HOME_VAR, home.path());
        let _privacy = EnvVarGuard::set("DISABLE_ERROR_REPORTING", "1");
        let _traffic = EnvVarGuard::set("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
        log::_reset_error_log_for_testing();
        log::log_mcp_debug("docs:test", "before startup");
        let path = get_mcp_logs_path("docs:test").unwrap();
        assert!(!path.exists());
        crate::utils::sinks::init_sinks().unwrap();
        crate::utils::sinks::init_sinks().unwrap();
        let message = crate::types::message::AttachmentMessage::new(json!({
            "type":"mcp_resource", "server":"docs:test", "uri":"mcp://missing", "name":"missing",
            "content":{"contents":[{"unknown":true}]}
        }));
        let messages = crate::utils::messages::normalize_attachment_for_api(&message, None);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, vec![crate::types::message::UserContent::MetaText(
            "<system-reminder>\n<mcp-resource server=\"docs:test\" uri=\"mcp://missing\">(No displayable content)</mcp-resource>\n</system-reminder>".into()
        )]);
        // Flush uses the same registered cleanup as the executable exit bridge.
        crate::utils::cleanup_registry::run_cleanup_functions_sync();
        let records = read_records(&path);
        assert_eq!(
            records.len(),
            2,
            "startup must not duplicate its queued snapshot"
        );
        assert_eq!(records[0]["debug"], "before startup");
        assert_eq!(
            records[1]["debug"],
            "No displayable content found in MCP resource mcp://missing."
        );
        for record in &records {
            assert_eq!(record.as_object().unwrap().len(), 4);
            assert_eq!(
                record["sessionId"],
                crate::bootstrap::state::get_session_id()
            );
            assert_eq!(
                record["cwd"],
                std::env::current_dir().unwrap().to_string_lossy().as_ref()
            );
            assert!(
                chrono::DateTime::parse_from_rfc3339(record["timestamp"].as_str().unwrap()).is_ok()
            );
        }
        // `just test --success-output immediate <name>` retains concrete runtime
        // rows as evidence; no diagnostic hook is added to production code.
        println!(
            "MCP_LOG_RUNTIME_PROOF {}",
            json!({"path":path,"rows":records})
        );
    }

    // CC messages.ts:3875-3949 logs only nonempty, untransformable resources.
    #[test]
    fn mcp_resource_log_matches_official_no_content_and_displayable_silence() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let home = TempDir::new();
        let _home = EnvVarGuard::set(HOME_VAR, home.path());
        log::_reset_error_log_for_testing();
        crate::utils::sinks::init_sinks().unwrap();
        for contents in [json!([]), json!([{"text":""}]), json!([{"blob":"AA=="}])] {
            let message = crate::types::message::AttachmentMessage::new(json!({
                "type":"mcp_resource","server":"silent","uri":"mcp://visible","name":"visible",
                "content":{"contents":contents}
            }));
            crate::utils::messages::normalize_attachment_for_api(&message, None);
        }
        crate::utils::cleanup_registry::run_cleanup_functions_sync();
        assert!(!get_mcp_logs_path("silent").unwrap().exists());
    }

    // CC errorLogSink.ts:179-195 distinguishes Error.stack from String(unknown).
    #[test]
    fn mcp_error_rows_match_official_error_stack_unknown_values_and_empty_stack() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let home = TempDir::new();
        let _home = EnvVarGuard::set(HOME_VAR, home.path());
        let error = |stack| LogError {
            name: "TypeError".into(),
            message: "message".into(),
            stack,
            axios: None,
        };
        for value in [
            McpLogError::Error(error(Some("native stack".into()))),
            McpLogError::Error(error(Some(String::new()))),
            McpLogError::Value(json!({"message":"plain object"})),
            McpLogError::Undefined,
        ] {
            log_mcp_error_impl("errors", value).unwrap();
        }
        _flush_log_writers_for_testing().unwrap();
        let rows = read_records(&get_mcp_logs_path("errors").unwrap());
        assert_eq!(
            rows.iter()
                .map(|row| row["error"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["native stack", "message", "[object Object]", "undefined"]
        );
        assert!(rows.iter().all(|row| row.as_object().unwrap().len() == 4));
    }

    // CC errorLogSink.ts:111-174: USER_TYPE gates only ordinary errors; Axios
    // details are enriched only when the original config.url is truthy.
    #[test]
    fn ordinary_error_rows_match_official_audience_axios_context_and_metadata() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let home = TempDir::new();
        let _home = EnvVarGuard::set(HOME_VAR, home.path());
        let error = LogError {
            name: "AxiosError".into(),
            message: "failed".into(),
            stack: Some(String::new()),
            axios: Some(AxiosErrorContext {
                url: Some("https://example.invalid/mcp".into()),
                status: Some(0),
                data: Some(json!({"error":{"message":"denied"}})),
            }),
        };
        log_error_impl(error).unwrap();
        _flush_log_writers_for_testing().unwrap();
        if !crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::TelemetryPayloads,
        ) {
            assert!(!get_errors_path().unwrap().exists());
            return;
        }
        let rows = read_records(&get_errors_path().unwrap());
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["error"],
            "[url=https://example.invalid/mcp,status=0,body=denied] failed"
        );
        assert_eq!(rows[0]["userType"], "ant");
        assert_eq!(rows[0]["version"], crate::constants::product::VERSION);
        assert_eq!(rows[0].as_object().unwrap().len(), 6);
    }

    // CC errorLogSink.ts:128-147 string/top-level/nested precedence.
    #[test]
    fn server_message_extraction_matches_official_shapes_and_precedence() {
        for (data, expected) in [
            (json!("text"), Some("text")),
            (json!({"message":"","error":{"message":"nested"}}), Some("")),
            (
                json!({"message":false,"error":{"message":"nested"}}),
                Some("nested"),
            ),
            (json!({"error":{"message":1}}), None),
            (json!(null), None),
        ] {
            assert_eq!(extract_server_message(&data), expected);
        }
    }

    // CC errorLogSink.ts:85-109 caches by full path and retries after mkdir.
    #[test]
    fn jsonl_writer_matches_official_path_cache_append_and_dispose() {
        let dir = TempDir::new();
        let path = dir.path().join("nested").join("output.jsonl");
        let writer = get_log_writer(&path).unwrap();
        writer.write(&json!({"value":"line\nvalue"})).unwrap();
        get_log_writer(&path)
            .unwrap()
            .write(&json!({"value":"second"}))
            .unwrap();
        writer.dispose().unwrap();
        assert_eq!(
            read_records(&path),
            [json!({"value":"line\nvalue"}), json!({"value":"second"})]
        );
        writer.write(&json!({"value":"after dispose"})).unwrap();
        writer.flush().unwrap();
        assert_eq!(
            read_records(&path).len(),
            3,
            "source dispose flushes without closing the writer"
        );
        let bad_path = dir.path().join("existing-directory");
        std::fs::create_dir(&bad_path).unwrap();
        let broken = get_log_writer(&bad_path).unwrap();
        broken.write(&json!({"error":true})).unwrap();
        assert!(broken.flush().is_err());
    }
}
