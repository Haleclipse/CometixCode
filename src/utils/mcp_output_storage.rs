//! MCP/tool binary-output storage helpers.
//!
//! Maps to: CC `utils/mcpOutputStorage.ts`.
//!
//! The official file is shared by MCP result persistence and WebFetch binary
//! downloads. Cometix keeps transcript JSONL writes disabled, but saving raw
//! binary bytes into the session `tool-results/` directory is a tool output
//! side effect rather than a transcript write, and matches the WebFetch binary
//! persistence path in Claude Code.

use std::path::PathBuf;

/// Maps to CC `utils/mcpOutputStorage.ts` `MCPResultType` consumer in
/// `getFormatDescription(...)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpResultType {
    ToolResult,
    StructuredContent,
    ContentArray,
}

/// Maps to CC `getFormatDescription(...)`.
pub fn get_format_description(result_type: McpResultType, schema: Option<&str>) -> String {
    match result_type {
        McpResultType::ToolResult => "Plain text".to_string(),
        McpResultType::StructuredContent => schema
            .map(|schema| format!("JSON with schema: {schema}"))
            .unwrap_or_else(|| "JSON".to_string()),
        McpResultType::ContentArray => schema
            .map(|schema| format!("JSON array with schema: {schema}"))
            .unwrap_or_else(|| "JSON array".to_string()),
    }
}

/// Maps to CC `getLargeOutputInstructions(...)`.
pub fn get_large_output_instructions(
    raw_output_path: &str,
    content_length: usize,
    format_description: &str,
    max_read_length: Option<usize>,
) -> String {
    let mut instructions = format!(
        "Error: result ({} characters) exceeds maximum allowed tokens. Output has been saved to {raw_output_path}.\nFormat: {format_description}\nUse offset and limit parameters to read specific portions of the file, search within it for specific content, and jq to make structured queries.\nREQUIREMENTS FOR SUMMARIZATION/ANALYSIS/REVIEW:\n- You MUST read the content from the file at {raw_output_path} in sequential chunks until 100% of the content has been read.\n",
        format_us_locale_number(content_length)
    );
    if let Some(max_read_length) = max_read_length {
        instructions.push_str(&format!(
            "- If you receive truncation warnings when reading the file (\"[N lines truncated]\"), reduce the chunk size until you have read 100% of the content without truncation ***DO NOT PROCEED UNTIL YOU HAVE DONE THIS***. Bash output is limited to {} chars.\n",
            format_us_locale_number(max_read_length)
        ));
    } else {
        instructions.push_str(
            "- If you receive truncation warnings when reading the file, reduce the chunk size until you have read 100% of the content without truncation.\n",
        );
    }
    instructions.push_str(
        "- Before producing ANY summary or analysis, you MUST explicitly describe what portion of the content you have read. ***If you did not read the entire content, you MUST explicitly state this.***\n",
    );
    instructions
}

fn format_us_locale_number(value: usize) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (idx, ch) in digits.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Maps to CC `extensionForMimeType(...)`.
pub fn extension_for_mime_type(mime_type: Option<&str>) -> &'static str {
    let Some(mime_type) = mime_type else {
        return "bin";
    };
    let mt = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match mt.as_str() {
        "application/pdf" => "pdf",
        "application/json" => "json",
        "text/csv" => "csv",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/markdown" => "md",
        "application/zip" => "zip",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/msword" => "doc",
        "application/vnd.ms-excel" => "xls",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/ogg" => "ogg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

/// Maps to CC `isBinaryContentType(...)`.
pub fn is_binary_content_type(content_type: &str) -> bool {
    if content_type.is_empty() {
        return false;
    }
    let mt = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if mt.starts_with("text/") {
        return false;
    }
    if mt.ends_with("+json") || mt == "application/json" {
        return false;
    }
    if mt.ends_with("+xml") || mt == "application/xml" {
        return false;
    }
    if mt.starts_with("application/javascript") {
        return false;
    }
    if mt == "application/x-www-form-urlencoded" {
        return false;
    }
    true
}

/// Maps to CC `PersistBinaryResult` success arm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistBinaryContentResult {
    pub filepath: PathBuf,
    pub size: usize,
    pub ext: String,
}

/// Maps to CC `PersistBinaryResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistBinaryResult {
    Saved(PersistBinaryContentResult),
    Error { error: String },
}

/// Maps to CC `persistBinaryContent(...)`.
pub fn persist_binary_content(
    bytes: &[u8],
    mime_type: Option<&str>,
    persist_id: &str,
) -> PersistBinaryResult {
    crate::utils::tool_result_storage::ensure_tool_results_dir();
    let ext = extension_for_mime_type(mime_type).to_string();
    let filepath = crate::utils::tool_result_storage::get_tool_results_dir()
        .join(format!("{persist_id}.{ext}"));

    if let Err(error) = std::fs::write(&filepath, bytes) {
        tracing::error!(?error, ?filepath, "failed to persist binary tool output");
        return PersistBinaryResult::Error {
            error: error.to_string(),
        };
    }

    // CC logs `tengu_binary_content_persisted` here. Telemetry is intentionally
    // not emitted by Cometix; the fixed-vocabulary mime/ext strings and size are
    // preserved in the return value for callers that need UI/result copy.
    PersistBinaryResult::Saved(PersistBinaryContentResult {
        filepath,
        size: bytes.len(),
        ext,
    })
}

/// Maps to CC `getBinaryBlobSavedMessage(...)`.
pub fn get_binary_blob_saved_message(
    filepath: &str,
    mime_type: Option<&str>,
    size: usize,
    source_description: &str,
) -> String {
    let mt = mime_type.unwrap_or("unknown type");
    format!(
        "{source_description}Binary content ({mt}, {}) saved to {filepath}",
        crate::utils::format::format_file_size(size as u64)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn mcp_output_storage_format_description_matches_official_copy() {
        assert_eq!(
            get_format_description(McpResultType::ToolResult, Some("ignored")),
            "Plain text"
        );
        assert_eq!(
            get_format_description(McpResultType::StructuredContent, None),
            "JSON"
        );
        assert_eq!(
            get_format_description(McpResultType::StructuredContent, Some("schema")),
            "JSON with schema: schema"
        );
        assert_eq!(
            get_format_description(McpResultType::ContentArray, Some("schema")),
            "JSON array with schema: schema"
        );
    }

    #[test]
    fn large_output_instructions_match_official_requirements() {
        let instructions =
            get_large_output_instructions("/tmp/out.json", 12345, "JSON", Some(50_000));
        assert!(instructions.contains("result (12,345 characters)"));
        assert!(instructions.contains("Output has been saved to /tmp/out.json"));
        assert!(instructions.contains("Bash output is limited to 50,000 chars"));
        assert!(instructions.contains("You MUST read the content from the file"));
        assert!(instructions.contains("Before producing ANY summary or analysis"));
    }

    #[test]
    fn mime_extension_and_binary_detection_match_official_table() {
        assert_eq!(extension_for_mime_type(Some("application/pdf")), "pdf");
        assert_eq!(
            extension_for_mime_type(Some("text/markdown; charset=utf-8")),
            "md"
        );
        assert_eq!(extension_for_mime_type(Some("image/jpeg")), "jpg");
        assert_eq!(
            extension_for_mime_type(Some("application/octet-stream")),
            "bin"
        );
        assert_eq!(extension_for_mime_type(None), "bin");

        assert!(is_binary_content_type("application/pdf"));
        assert!(is_binary_content_type(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        ));
        assert!(!is_binary_content_type("text/plain"));
        assert!(!is_binary_content_type("application/json"));
        assert!(!is_binary_content_type("application/vnd.api+json"));
        assert!(!is_binary_content_type("application/xml"));
        assert!(!is_binary_content_type("application/javascript"));
        assert!(!is_binary_content_type("application/x-www-form-urlencoded"));
    }

    #[test]
    fn persist_binary_content_writes_raw_bytes_to_tool_results_dir() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let root = std::env::temp_dir().join(format!(
            "cometix-mcp-output-storage-{}",
            uuid::Uuid::new_v4()
        ));
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(&root);
        crate::bootstrap::state::set_original_cwd("/tmp/cometix-project");
        crate::bootstrap::state::set_session_id("session-binary-test");

        let result = persist_binary_content(b"%PDF-raw", Some("application/pdf"), "webfetch-test");
        let saved = match result {
            PersistBinaryResult::Saved(saved) => saved,
            PersistBinaryResult::Error { error } => panic!("unexpected persist error: {error}"),
        };
        assert_eq!(saved.size, 8);
        assert_eq!(saved.ext, "pdf");
        assert!(saved.filepath.ends_with("webfetch-test.pdf"));
        assert_eq!(std::fs::read(&saved.filepath).unwrap(), b"%PDF-raw");

        crate::bootstrap::state::set_original_cwd(previous_cwd);
        crate::bootstrap::state::set_session_id(previous_session_id);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn binary_blob_saved_message_matches_official_copy() {
        assert_eq!(
            get_binary_blob_saved_message("/tmp/file.pdf", Some("application/pdf"), 2048, ""),
            "Binary content (application/pdf, 2KB) saved to /tmp/file.pdf"
        );
    }
}
