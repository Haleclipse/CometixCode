//! Passive LSP diagnostic feedback helpers.
//!
//! Maps to: CC `services/lsp/passiveFeedback.ts`.
//!
//! This module registers `textDocument/publishDiagnostics` handlers on every
//! configured LSP server and converts incoming notifications into Claude's
//! passive diagnostic attachment queue.

use crate::services::lsp::diagnostic_registry::register_pending_lsp_diagnostic;
use crate::services::lsp::server_manager::LspServerManager;
use crate::services::lsp::types::{
    Diagnostic, DiagnosticFile, DiagnosticPosition, DiagnosticRange,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspDiagnosticPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspDiagnosticRange {
    pub start: LspDiagnosticPosition,
    pub end: LspDiagnosticPosition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspDiagnostic {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<u8>,
    pub range: LspDiagnosticRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<serde_json::Value>,
}

/// Maps to: CC `PublishDiagnosticsParams` consumed by
/// `formatDiagnosticsForAttachment(...)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishDiagnosticsParams {
    pub uri: String,
    pub diagnostics: Vec<LspDiagnostic>,
}

/// Maps to: CC `HandlerRegistrationResult.registrationErrors[]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerRegistrationError {
    pub server_name: String,
    pub error: String,
}

/// Maps to: CC `HandlerRegistrationResult.diagnosticFailures` values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticFailure {
    pub count: u32,
    pub last_error: String,
}

/// Maps to: CC `HandlerRegistrationResult`.
#[derive(Clone, Debug)]
pub struct HandlerRegistrationResult {
    pub total_servers: usize,
    pub success_count: usize,
    pub registration_errors: Vec<HandlerRegistrationError>,
    pub diagnostic_failures: Arc<Mutex<BTreeMap<String, DiagnosticFailure>>>,
}

/// Maps to: CC `registerLSPNotificationHandlers(manager)`.
///
/// `&LspServerManager` (not `&mut`): CC calls this with the shared singleton
/// from inside `initializeLspServerManager`'s `.then(...)` (`manager.ts:190`),
/// while every other caller still holds the same reference.
pub fn register_lsp_notification_handlers(manager: &LspServerManager) -> HandlerRegistrationResult {
    let mut total_servers = 0;
    let mut success_count = 0;
    let registration_errors = Vec::new();
    let diagnostic_failures = Arc::new(Mutex::new(BTreeMap::new()));

    for (server_name, server) in manager.get_all_servers() {
        total_servers += 1;
        let server_name_for_handler = server_name.clone();
        let failures_for_handler = diagnostic_failures.clone();
        server.on_notification("textDocument/publishDiagnostics", move |params| {
            match serde_json::from_value::<PublishDiagnosticsParams>(params) {
                Ok(params) => {
                    let diagnostic_files = format_diagnostics_for_attachment(&params);
                    if diagnostic_files
                        .first()
                        .is_none_or(|file| file.diagnostics.is_empty())
                    {
                        return;
                    }
                    register_pending_lsp_diagnostic(
                        server_name_for_handler.clone(),
                        diagnostic_files,
                    );
                    failures_for_handler
                        .lock()
                        .unwrap()
                        .remove(&server_name_for_handler);
                }
                Err(error) => {
                    let mut failures = failures_for_handler.lock().unwrap();
                    let failure = failures
                        .entry(server_name_for_handler.clone())
                        .or_insert_with(DiagnosticFailure::default);
                    failure.count = failure.count.saturating_add(1);
                    failure.last_error = error.to_string();
                }
            }
        });
        success_count += 1;
    }

    HandlerRegistrationResult {
        total_servers,
        success_count,
        registration_errors,
        diagnostic_failures,
    }
}

/// Maps to: CC `mapLSPSeverity(...)`.
pub fn map_lsp_severity(lsp_severity: Option<u8>) -> &'static str {
    match lsp_severity {
        Some(1) => "Error",
        Some(2) => "Warning",
        Some(3) => "Info",
        Some(4) => "Hint",
        _ => "Error",
    }
}

/// Maps to: CC `formatDiagnosticsForAttachment(...)`.
pub fn format_diagnostics_for_attachment(params: &PublishDiagnosticsParams) -> Vec<DiagnosticFile> {
    let uri = file_uri_to_path(&params.uri).unwrap_or_else(|| params.uri.clone());
    let diagnostics = params
        .diagnostics
        .iter()
        .map(|diag| Diagnostic {
            message: diag.message.clone(),
            severity: map_lsp_severity(diag.severity).to_string(),
            range: DiagnosticRange {
                start: DiagnosticPosition {
                    line: diag.range.start.line,
                    character: diag.range.start.character,
                },
                end: DiagnosticPosition {
                    line: diag.range.end.line,
                    character: diag.range.end.character,
                },
            },
            source: diag.source.clone(),
            code: diag.code.as_ref().and_then(lsp_code_to_string),
        })
        .collect();

    vec![DiagnosticFile { uri, diagnostics }]
}

/// Minimal registration seam for a single already-received notification.
/// Maps to the body of CC `registerLSPNotificationHandlers(...)` after the
/// server-specific `onNotification(...)` callback receives validated params.
pub fn register_lsp_diagnostic_notification(server_name: &str, params: &PublishDiagnosticsParams) {
    let diagnostic_files = format_diagnostics_for_attachment(params);
    if diagnostic_files
        .first()
        .is_none_or(|file| file.diagnostics.is_empty())
    {
        return;
    }
    register_pending_lsp_diagnostic(server_name.to_string(), diagnostic_files);
}

fn lsp_code_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => None,
    }
}

fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let mut path = if rest.starts_with('/') {
        rest.to_string()
    } else {
        format!("/{rest}")
    };
    if path.len() >= 4
        && path.as_bytes()[0] == b'/'
        && path.as_bytes()[2] == b':'
        && path.as_bytes()[1].is_ascii_alphabetic()
    {
        path.remove(0);
    }
    percent_decode(&path)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_value(bytes[index + 1])?;
            let lo = hex_value(bytes[index + 2])?;
            out.push((hi << 4) | lo);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::lsp::diagnostic_registry::{
        check_for_lsp_diagnostics, reset_all_lsp_diagnostic_state,
    };
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn params(
        uri: &str,
        severity: Option<u8>,
        code: serde_json::Value,
    ) -> PublishDiagnosticsParams {
        PublishDiagnosticsParams {
            uri: uri.to_string(),
            diagnostics: vec![LspDiagnostic {
                message: "broken".to_string(),
                severity,
                range: LspDiagnosticRange {
                    start: LspDiagnosticPosition {
                        line: 2,
                        character: 3,
                    },
                    end: LspDiagnosticPosition {
                        line: 2,
                        character: 8,
                    },
                },
                source: Some("server".to_string()),
                code: Some(code),
            }],
        }
    }

    #[test]
    fn lsp_severity_mapping_matches_official_defaults() {
        assert_eq!(map_lsp_severity(Some(1)), "Error");
        assert_eq!(map_lsp_severity(Some(2)), "Warning");
        assert_eq!(map_lsp_severity(Some(3)), "Info");
        assert_eq!(map_lsp_severity(Some(4)), "Hint");
        assert_eq!(map_lsp_severity(None), "Error");
        assert_eq!(map_lsp_severity(Some(9)), "Error");
    }

    #[test]
    fn format_diagnostics_for_attachment_decodes_file_uri_and_code() {
        let files = format_diagnostics_for_attachment(&params(
            "file:///tmp/project/src%20main.rs",
            Some(2),
            serde_json::json!(123),
        ));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].uri, "/tmp/project/src main.rs");
        assert_eq!(files[0].diagnostics[0].severity, "Warning");
        assert_eq!(files[0].diagnostics[0].code.as_deref(), Some("123"));
        assert_eq!(files[0].diagnostics[0].range.start.line, 2);
    }

    #[test]
    fn malformed_file_uri_falls_back_to_original_uri_like_official() {
        let files = format_diagnostics_for_attachment(&params(
            "file:///tmp/%zz.rs",
            Some(1),
            serde_json::Value::Null,
        ));
        assert_eq!(files[0].uri, "file:///tmp/%zz.rs");
        assert_eq!(files[0].diagnostics[0].code, None);
    }

    #[test]
    fn notification_registration_adds_non_empty_diagnostics_to_registry() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();
        register_lsp_diagnostic_notification(
            "rust",
            &params("file:///tmp/a.rs", Some(1), serde_json::json!("E1")),
        );
        let delivered = check_for_lsp_diagnostics();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].server_name, "rust");
        assert_eq!(delivered[0].files[0].diagnostics[0].message, "broken");
        reset_all_lsp_diagnostic_state();
    }
}
