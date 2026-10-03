//! Incremental port of official `tools/LSPTool/*`.
//!
//! Schema/prompt metadata maps to CC `tools/LSPTool/LSPTool.ts` and
//! `tools/LSPTool/prompt.ts`. Tool execution routes through
//! `services/lsp/manager.rs`; operation-specific response formatting lives in
//! the official `formatters.ts`-mapped module.

pub mod formatters;
pub mod prompt;
pub mod schemas;
pub mod symbol_context;
pub mod ui;

/// Maps to: CC `tools.ts` gate `isEnvTruthy(process.env.ENABLE_LSP_TOOL)`.
pub fn is_lsp_tool_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("ENABLE_LSP_TOOL").as_deref())
}

/// Maps to: CC `LSPTool.ts:59-85` `inputSchema`.
///
/// `line` / `character` are `z.number().int().positive()`; the safe-integer
/// `maximum` comes from `.int()`, not from a hand-added cap.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "operation",
                zod::enumeration(prompt::OPERATIONS.to_vec())
                    .describe("The LSP operation to perform"),
            ),
            (
                "filePath",
                zod::string().describe("The absolute or relative path to the file"),
            ),
            (
                "line",
                zod::number()
                    .int()
                    .positive()
                    .describe("The line number (1-based, as shown in editors)"),
            ),
            (
                "character",
                zod::number()
                    .int()
                    .positive()
                    .describe("The character offset (1-based, as shown in editors)"),
            ),
        ])
    })
}

/// Maps to: CC `LSPTool` metadata.
pub fn lsp_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::LSP_TOOL_NAME.to_string(),
        description: prompt::DESCRIPTION.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/LSPTool/LSPTool.ts` `export type Output` (:124) — outputSchema
/// (:89-121).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Output {
    pub(crate) operation: String,
    pub(crate) result: String,
    pub(crate) file_path: String,
    pub(crate) result_count: Option<usize>,
    pub(crate) file_count: Option<usize>,
}

/// Maps to: CC `tools/LSPTool/LSPTool.ts` `validateInput(...)` return shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LspValidationResult {
    pub(crate) result: bool,
    pub(crate) message: Option<String>,
    pub(crate) error_code: Option<u32>,
}

impl LspValidationResult {
    fn ok() -> Self {
        Self {
            result: true,
            message: None,
            error_code: None,
        }
    }

    fn error(message: impl Into<String>, error_code: u32) -> Self {
        Self {
            result: false,
            message: Some(message.into()),
            error_code: Some(error_code),
        }
    }
}

fn is_unc_path_for_lsp(path: &str) -> bool {
    // Maps to: CC `validateInput(...)` UNC guard that avoids filesystem access
    // for paths beginning with `\\` or `//` to prevent NTLM credential leaks.
    path.starts_with("\\\\") || path.starts_with("//")
}

/// Maps to: CC `tools/LSPTool/LSPTool.ts` `validateInput(...)`.
pub(crate) async fn validate_lsp_input(
    input: &serde_json::Value,
    cwd: &std::path::Path,
) -> LspValidationResult {
    let parsed = match schemas::parse_lsp_tool_input(input) {
        Ok(parsed) => parsed,
        Err(error) => {
            return LspValidationResult::error(format!("Invalid input: {error}"), 3);
        }
    };
    if is_unc_path_for_lsp(&parsed.file_path) {
        return LspValidationResult::ok();
    }
    let absolute_path = expand_lsp_file_path(&parsed.file_path, cwd);
    if is_unc_path_for_lsp(&absolute_path) {
        return LspValidationResult::ok();
    }
    match tokio::fs::metadata(&absolute_path).await {
        Ok(metadata) if metadata.is_file() => LspValidationResult::ok(),
        Ok(_) => LspValidationResult::error(format!("Path is not a file: {}", parsed.file_path), 2),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            LspValidationResult::error(format!("File does not exist: {}", parsed.file_path), 1)
        }
        Err(error) => LspValidationResult::error(
            format!("Cannot access file: {}. {error}", parsed.file_path),
            4,
        ),
    }
}

/// Maps to: CC `tools/LSPTool/LSPTool.ts` `call` fallback/error output
/// creation. The async path below owns manager routing.
pub(crate) fn lsp_output(input: &serde_json::Value) -> Output {
    match schemas::parse_lsp_tool_input(input) {
        Ok(parsed) => Output {
            operation: parsed.operation.as_str().to_string(),
            result: "LSP server manager not initialized. This may indicate a startup issue."
                .to_string(),
            file_path: parsed.file_path,
            result_count: None,
            file_count: None,
        },
        Err(error) => Output {
            operation: input
                .get("operation")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
            result: format!("Invalid input: {error}"),
            file_path: input
                .get("filePath")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
            result_count: None,
            file_count: None,
        },
    }
}

/// Maps to: CC `tools/LSPTool/LSPTool.ts` `call(...)`.
///
/// `cwd` is CC's `getCwd()` (`:226`), which is also what `expandPath(...)`
/// resolves relative paths against (`:225`, `utils/path.ts:33`). This port's
/// `getCwd()` is `ToolUseContext::effective_cwd()` (`tool.rs:1607` — the
/// `cwd_override` async-local stand-in, falling back to the startup cwd), and
/// it is the same value `check_permissions` already uses. Passing the PROCESS
/// cwd here instead meant a relative `filePath` was permission-checked against
/// one directory and opened, queried and `git check-ignore`d against another
/// under a worktree, `--add-dir`, or a subagent `cwd_override`.
async fn lsp_output_via_manager(input: &serde_json::Value, cwd: &std::path::Path) -> Output {
    let parsed = match schemas::parse_lsp_tool_input(input) {
        Ok(parsed) => parsed,
        Err(_) => return lsp_output(input),
    };

    if matches!(
        crate::services::lsp::manager::get_initialization_status(),
        crate::services::lsp::manager::InitializationStatus::Pending
    ) {
        crate::services::lsp::manager::wait_for_initialization().await;
    }

    if !crate::services::lsp::manager::has_lsp_server_manager() {
        return Output {
            operation: parsed.operation.as_str().to_string(),
            result: "LSP server manager not initialized. This may indicate a startup issue."
                .to_string(),
            file_path: parsed.file_path,
            result_count: None,
            file_count: None,
        };
    }

    let absolute_path = expand_lsp_file_path(&parsed.file_path, cwd);
    let mapping = schemas::get_method_and_params(&parsed, std::path::Path::new(&absolute_path));

    if !crate::services::lsp::manager::is_file_open(&absolute_path) {
        match tokio::fs::metadata(&absolute_path).await {
            Ok(metadata) if metadata.len() > MAX_LSP_FILE_SIZE_BYTES => {
                return Output {
                    operation: parsed.operation.as_str().to_string(),
                    result: format!(
                        "File too large for LSP analysis ({}MB exceeds 10MB limit)",
                        metadata.len().div_ceil(1_000_000)
                    ),
                    file_path: parsed.file_path,
                    result_count: None,
                    file_count: None,
                };
            }
            Ok(_) => match tokio::fs::read_to_string(&absolute_path).await {
                Ok(content) => {
                    if let Err(error) =
                        crate::services::lsp::manager::open_file(&absolute_path, content).await
                    {
                        return lsp_error_output(&parsed, error);
                    }
                }
                Err(error) => return lsp_error_output(&parsed, error),
            },
            Err(error) => return lsp_error_output(&parsed, error),
        }
    }

    match crate::services::lsp::manager::send_request(
        &absolute_path,
        mapping.method,
        mapping.params,
    )
    .await
    {
        Ok(Some(mut value)) => {
            if matches!(
                parsed.operation,
                schemas::LspOperation::IncomingCalls | schemas::LspOperation::OutgoingCalls
            ) {
                if value.as_array().is_none_or(|items| items.is_empty()) {
                    return Output {
                        operation: parsed.operation.as_str().to_string(),
                        result: "No call hierarchy item found at this position".to_string(),
                        file_path: parsed.file_path,
                        result_count: Some(0),
                        file_count: Some(0),
                    };
                }
                value = match resolve_call_hierarchy_calls(&absolute_path, parsed.operation, value)
                    .await
                {
                    Ok(value) => value,
                    Err(error) => return lsp_error_output(&parsed, error),
                };
            }
            // CC `:226` `const cwd = getCwd()`, used for both the gitignore
            // filter (`:351`) and the formatter (`:377-381`).
            let cwd = cwd.display().to_string();
            value = formatters::filter_git_ignored_result(parsed.operation, value, &cwd).await;
            let formatted = formatters::format_result(parsed.operation, &value, Some(&cwd));
            Output {
                operation: parsed.operation.as_str().to_string(),
                result: formatted.formatted,
                file_path: parsed.file_path,
                result_count: Some(formatted.result_count),
                file_count: Some(formatted.file_count),
            }
        }
        Ok(None) => Output {
            operation: parsed.operation.as_str().to_string(),
            result: format!(
                "No LSP server available for file type: {}",
                std::path::Path::new(&absolute_path)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| format!(".{ext}"))
                    .unwrap_or_default()
            ),
            file_path: parsed.file_path,
            result_count: None,
            file_count: None,
        },
        Err(error) => lsp_error_output(&parsed, error),
    }
}

const MAX_LSP_FILE_SIZE_BYTES: u64 = 10_000_000;

/// Maps to: CC `tools/LSPTool/LSPTool.ts:153,168,225` `expandPath(filePath)`.
///
/// CC's `expandPath` resolves relative paths against `baseDir ?? getCwd()`
/// (`utils/path.ts:33`) — the async-local override, never `process.cwd()`. All
/// four LSPTool sites therefore share one base directory; here they share
/// `cwd`.
fn expand_lsp_file_path(file_path: &str, cwd: &std::path::Path) -> String {
    crate::utils::path::expand_path(file_path, Some(cwd))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| file_path.to_string())
}

/// The context-free stand-in for `getCwd()`, for the one trait method the port
/// does not thread a `ToolUseContext` through (`get_path`). Same choice as
/// `file_read_tool/mod.rs:1721`.
fn lsp_tool_default_cwd() -> std::path::PathBuf {
    crate::bootstrap::state::get_original_cwd()
}

async fn resolve_call_hierarchy_calls(
    absolute_path: &str,
    operation: schemas::LspOperation,
    prepared_items: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let Some(items) = prepared_items.as_array() else {
        return Ok(serde_json::Value::Null);
    };
    let Some(item) = items.first() else {
        return Ok(serde_json::Value::Null);
    };
    let method = match operation {
        schemas::LspOperation::IncomingCalls => "callHierarchy/incomingCalls",
        schemas::LspOperation::OutgoingCalls => "callHierarchy/outgoingCalls",
        _ => return Ok(prepared_items),
    };
    Ok(crate::services::lsp::manager::send_request(
        absolute_path,
        method,
        serde_json::json!({ "item": item }),
    )
    .await?
    .unwrap_or(serde_json::Value::Null))
}

fn lsp_error_output(error_input: &schemas::LspToolInput, error: impl std::fmt::Display) -> Output {
    Output {
        operation: error_input.operation.as_str().to_string(),
        result: format!(
            "Error performing {}: {}",
            error_input.operation.as_str(),
            error
        ),
        file_path: error_input.file_path.clone(),
        result_count: None,
        file_count: None,
    }
}

/// Behavioral half of CC `LspTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct LspTool;

impl crate::tool::ToolCall for LspTool {
    fn name(&self) -> &'static str {
        "LSP"
    }

    /// Maps to: CC `LSPTool.ts:218-220` `async prompt() { return DESCRIPTION }`
    /// — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::DESCRIPTION.to_string()
    }

    /// Maps to: CC `LSPTool.isEnabled()` → `isLspConnected()`.
    fn is_enabled(&self) -> bool {
        crate::services::lsp::manager::is_lsp_connected()
    }

    /// Maps to: CC `LSPTool.isConcurrencySafe(...)`.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("code intelligence (definitions, references, symbols, hover)")
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `LSPTool.ts:132-134` `async description() { return
    /// DESCRIPTION }`, read at `hooks/useCanUseTool.tsx:138` to fill the
    /// permission dialog's description. Without it the trait default
    /// (`tool.rs:2058`) wrote an empty string into the request.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::DESCRIPTION.to_string()
    }

    fn get_path(&self, args: &serde_json::Value) -> Option<String> {
        let cwd = lsp_tool_default_cwd();
        args.get("filePath")
            .and_then(serde_json::Value::as_str)
            .map(|file_path| expand_lsp_file_path(file_path, &cwd))
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        let parsed = match schemas::parse_lsp_tool_input(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                return crate::tool::ValidationResult::error(format!("Invalid input: {error}"), 3);
            }
        };
        if is_unc_path_for_lsp(&parsed.file_path) {
            return crate::tool::ValidationResult::ok();
        }
        let absolute_path = expand_lsp_file_path(&parsed.file_path, &context.effective_cwd());
        if is_unc_path_for_lsp(&absolute_path) {
            return crate::tool::ValidationResult::ok();
        }
        match futures::executor::block_on(
            crate::utils::fs_operations::get_fs_implementation()
                .stat(std::path::Path::new(&absolute_path)),
        ) {
            Ok(metadata) if metadata.is_file() => crate::tool::ValidationResult::ok(),
            Ok(_) => crate::tool::ValidationResult::error(
                format!("Path is not a file: {}", parsed.file_path),
                2,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::tool::ValidationResult::error(
                    format!("File does not exist: {}", parsed.file_path),
                    1,
                )
            }
            Err(error) => crate::tool::ValidationResult::error(
                format!("Cannot access file: {}. {error}", parsed.file_path),
                4,
            ),
        }
    }

    /// Maps to: CC `tools/LSPTool/LSPTool.ts:210-217#checkPermissions`.
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let cwd = context.effective_cwd();
        let file_path = args
            .get("filePath")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| cwd.display().to_string());
        crate::utils::permissions::filesystem::check_read_permission_for_tool(
            &file_path,
            args,
            &context.tool_permission_context,
            &cwd,
        )
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        let cwd = context.effective_cwd();
        Box::pin(async move {
            let _ = request;
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::Lsp(lsp_output_via_manager(args, &cwd).await),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/LSPTool/LSPTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:415-421).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::Lsp(output) => (
                output.result.clone(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                String::new(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording LSPTool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::Lsp(output) => Some(ui::output_to_value(output)),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};

    #[test]
    fn lsp_tool_schema_matches_official_input_shape() {
        let schema = super::lsp_tool_schema();
        assert_eq!(schema.name, "LSP");
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!([
                "operation",
                "filePath",
                "line",
                "character"
            ]))
        );
        assert!(
            schema
                .input_schema
                .pointer("/properties/operation/enum")
                .and_then(|value| value.as_array())
                .is_some_and(|values| values.iter().any(|value| value == "findReferences"))
        );
        assert!(schema.description.contains("Language Server Protocol"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lsp_validate_input_matches_official_file_checks_and_unc_bypass() {
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-lsp-validate-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("main.rs");
        std::fs::write(&file_path, "fn main() {}\n").unwrap();
        let valid = serde_json::json!({
            "operation": "hover",
            "filePath": file_path,
            "line": 1,
            "character": 1,
        });
        assert_eq!(
            super::validate_lsp_input(&valid, &temp_dir).await,
            super::LspValidationResult::ok()
        );

        let missing = serde_json::json!({
            "operation": "hover",
            "filePath": temp_dir.join("missing.rs"),
            "line": 1,
            "character": 1,
        });
        let missing_result = super::validate_lsp_input(&missing, &temp_dir).await;
        assert!(!missing_result.result);
        assert_eq!(missing_result.error_code, Some(1));
        assert!(
            missing_result
                .message
                .unwrap()
                .contains("File does not exist")
        );

        let dir_input = serde_json::json!({
            "operation": "hover",
            "filePath": temp_dir,
            "line": 1,
            "character": 1,
        });
        let dir_result = super::validate_lsp_input(&dir_input, &temp_dir).await;
        assert!(!dir_result.result);
        assert_eq!(dir_result.error_code, Some(2));
        assert!(dir_result.message.unwrap().contains("Path is not a file"));

        let invalid = serde_json::json!({
            "operation": "rename",
            "filePath": "src/main.rs",
            "line": 1,
            "character": 1,
        });
        let invalid_result = super::validate_lsp_input(&invalid, &temp_dir).await;
        assert!(!invalid_result.result);
        assert_eq!(invalid_result.error_code, Some(3));
        assert!(invalid_result.message.unwrap().contains("Invalid input"));

        let unc = serde_json::json!({
            "operation": "hover",
            "filePath": "//server/share/file.rs",
            "line": 1,
            "character": 1,
        });
        assert_eq!(
            super::validate_lsp_input(&unc, &temp_dir).await,
            super::LspValidationResult::ok()
        );
        let _ = std::fs::remove_dir_all(
            valid["filePath"]
                .as_str()
                .and_then(|path| std::path::Path::new(path).parent())
                .unwrap(),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lsp_tool_call_returns_official_output_schema_and_model_copy() {
        use crate::tool::ToolCall;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::services::lsp::manager::reset_lsp_manager_for_testing();
        let args = serde_json::json!({
            "operation": "hover",
            "filePath": "/tmp/example.rs",
            "line": 1,
            "character": 1
        });
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-lsp".to_string(),
            "toolu_lsp".to_string(),
            "LSP".to_string(),
            "hover /tmp/example.rs".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = super::LspTool;
        let result = tool
            .call(
                &args,
                &request,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;

        let crate::tool::ToolOutput::Lsp(output) = result.data else {
            panic!("LSP should return its official ToolOutput variant");
        };
        assert_eq!(output.operation, "hover");
        assert_eq!(output.file_path, "/tmp/example.rs");
        assert!(output.result.contains("not initialized"));

        let data = crate::tool::ToolOutput::Lsp(output);
        let (content, status) = tool.map_tool_result_to_tool_result_block_param(&data, "toolu_lsp");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert!(content.contains("not initialized"));
        // No display shape — the trait projects the raw Output object.
        let raw = tool
            .tool_use_result(&data)
            .expect("raw output should ride the row");
        assert_eq!(raw.get("operation"), Some(&serde_json::json!("hover")));
        assert!(
            raw.get("result")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|result| result.contains("not initialized"))
        );
        assert_eq!(raw.get("resultCount"), None);
        assert_eq!(raw.get("fileCount"), None);
        crate::services::lsp::manager::reset_lsp_manager_for_testing();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lsp_tool_uses_initialized_empty_manager_for_no_server_result() {
        use crate::tool::ToolCall;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_SIMPLE");
        crate::services::lsp::manager::reset_lsp_manager_for_testing();
        crate::services::lsp::manager::initialize_lsp_server_manager();

        // The file must exist: CC opens it before routing and surfaces the
        // open failure as `Error performing {operation}: ...`.
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-lsp-no-server-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("example.rs");
        std::fs::write(&file_path, "fn main() {}\n").unwrap();
        let args = serde_json::json!({
            "operation": "hover",
            "filePath": file_path,
            "line": 1,
            "character": 1
        });
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-lsp-empty".to_string(),
            "toolu_lsp_empty".to_string(),
            "LSP".to_string(),
            "hover /tmp/example.rs".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = super::LspTool;
        let result = tool
            .call(
                &args,
                &request,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;

        let crate::tool::ToolOutput::Lsp(output) = result.data else {
            panic!("LSP should return its official ToolOutput variant");
        };
        assert_eq!(output.operation, "hover");
        assert_eq!(output.result, "No LSP server available for file type: .rs");
        crate::services::lsp::manager::reset_lsp_manager_for_testing();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn lsp_tool_schema_matches_the_zod_projection_oracle() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("../cc_input_schemas.json")).unwrap();
        assert_eq!(
            super::lsp_tool_schema().input_schema,
            oracle["LSPTool"]["schema"]
        );
    }

    #[test]
    fn expand_lsp_file_path_expands_home_notation() {
        // `~` expands to `homedir()`: pin it to a literal.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let home = "/home/someone";
        let _home = EnvVarGuard::set(HOME_VAR, home);
        assert_eq!(
            super::expand_lsp_file_path("~/project/main.rs", std::path::Path::new("/unused")),
            std::path::Path::new(&home)
                .join("project/main.rs")
                .display()
                .to_string()
        );
    }

    /// H5: CC resolves `expandPath(filePath)` at `getPath` (`:153`),
    /// `validateInput` (`:168`) and `call` (`:225`) plus `getCwd()` at `:226`
    /// against ONE base directory. In this port that base is
    /// `ToolUseContext::effective_cwd()` — the same value `check_permissions`
    /// already uses — so a relative `filePath` can no longer be permission-
    /// checked against one directory and opened against another.
    #[tokio::test(flavor = "current_thread")]
    async fn relative_paths_resolve_against_the_context_cwd_everywhere() {
        use crate::tool::ToolCall;

        let worktree =
            std::env::temp_dir().join(format!("cometix-lsp-cwd-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join("src/main.rs"), "fn main() {}\n").unwrap();
        let expected = worktree.join("src/main.rs").display().to_string();

        let args = serde_json::json!({
            "operation": "hover",
            "filePath": "src/main.rs",
            "line": 1,
            "character": 1
        });
        let context =
            crate::tool::ToolUseContext::default().with_cwd_override(Some(worktree.clone()));
        let tool = super::LspTool;

        // validateInput resolves against the context cwd (the file only exists
        // there), and the permission check agrees with it.
        assert_eq!(
            tool.validate_input(&args, &context),
            crate::tool::ValidationResult::Ok
        );
        assert_eq!(
            super::validate_lsp_input(&args, &context.effective_cwd()).await,
            super::LspValidationResult::ok()
        );
        assert_eq!(
            super::expand_lsp_file_path("src/main.rs", &context.effective_cwd()),
            expected
        );
        assert_eq!(context.effective_cwd(), worktree);

        // getPath has no context on the trait; it uses the port's context-free
        // `getCwd()` stand-in, which is the startup cwd, not the process cwd.
        assert_eq!(
            tool.get_path(&args),
            Some(
                crate::bootstrap::state::get_original_cwd()
                    .join("src/main.rs")
                    .display()
                    .to_string()
            )
        );

        let _ = std::fs::remove_dir_all(&worktree);
    }

    /// H2 consumer #1, end to end: CC `LSPTool.call` checks the status and
    /// awaits `waitForInitialization()` when it is `'pending'`
    /// (`LSPTool.ts:228-233`) — "This prevents returning 'no server available'
    /// before init completes". With a synchronous initialization that branch
    /// was unreachable and `wait_for_initialization()` was an empty function,
    /// so nothing pinned it. Here the tool call starts while initialization is
    /// genuinely in flight and must not answer "not initialized".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tool_call_waits_for_a_pending_initialization_like_official() {
        use crate::tool::ToolCall;

        crate::services::lsp::manager::reset_lsp_manager_for_testing();
        let (release_sender, release_receiver) = std::sync::mpsc::channel::<()>();
        let release_receiver = std::sync::Mutex::new(release_receiver);
        crate::services::lsp::manager::initialize_lsp_server_manager_for_testing(
            crate::services::lsp::server_manager::create_lsp_server_manager_with_config_loader(
                std::sync::Arc::new(move || {
                    let _ = release_receiver.lock().unwrap().recv();
                    Ok(crate::services::lsp::config::AllLspServers::default())
                }),
            ),
        );
        assert_eq!(
            crate::services::lsp::manager::get_initialization_status(),
            crate::services::lsp::manager::InitializationStatus::Pending
        );

        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-lsp-pending-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("example.rs");
        std::fs::write(&file_path, "fn main() {}\n").unwrap();
        let args = serde_json::json!({
            "operation": "hover",
            "filePath": file_path,
            "line": 1,
            "character": 1
        });
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-lsp-pending".to_string(),
            "toolu_lsp_pending".to_string(),
            "LSP".to_string(),
            "hover example.rs".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );

        let call = tokio::spawn(async move {
            let tool = super::LspTool;
            let context = crate::tool::ToolUseContext::default();
            tool.call(&args, &request, &context, None, None, None).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !call.is_finished(),
            "the tool must await waitForInitialization() while pending"
        );

        let _ = release_sender.send(());
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), call)
            .await
            .expect("the call must resolve once initialization settles")
            .unwrap();
        let crate::tool::ToolOutput::Lsp(output) = result.data else {
            panic!("LSP should return its official ToolOutput variant");
        };
        assert_eq!(
            output.result, "No LSP server available for file type: .rs",
            "the settled (empty) manager answers, not 'not initialized'"
        );

        crate::services::lsp::manager::reset_lsp_manager_for_testing();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// H6: CC `LSPTool.ts:132-134` returns the full DESCRIPTION for the
    /// permission dialog; the trait default returned an empty string.
    #[test]
    fn lsp_tool_description_fills_the_permission_dialog_like_official() {
        use crate::tool::ToolCall;
        let tool = super::LspTool;
        assert_eq!(
            tool.description(&serde_json::json!({})),
            super::prompt::DESCRIPTION
        );
        assert!(!tool.description(&serde_json::json!({})).is_empty());
        // userFacingName was already right on both sides.
        assert_eq!(tool.user_facing_name(None), "LSP");
    }
}
