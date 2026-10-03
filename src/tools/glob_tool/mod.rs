//! Glob tool metadata, execution, and UI.
//!
//! Maps to:
//! - CC `tools/GlobTool/GlobTool.ts` (schema + `call` execution)
//! - CC `tools/GlobTool/prompt.ts`
//! - CC `tools/GlobTool/UI.tsx`
//!
//! Execution delegates to the source-shaped, ripgrep-backed `utils::glob`
//! owner; Grep remains an independent Tool batch.

pub mod prompt;
pub mod ui;

/// Maps to: CC `GlobTool.ts:26-36` `inputSchema` — the carrier schema, the
/// single definition behind both validation and the JSON Schema projection.
///
/// Built once per process, matching CC's `lazySchema()` guarantee of one
/// schema instance per session.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "pattern",
                zod::string().describe("The glob pattern to match files against"),
            ),
            (
                "path",
                zod::string().optional().describe(
                    "The directory to search in. If not specified, the current working directory will be used. IMPORTANT: Omit this field to use the default directory. DO NOT enter \"undefined\" or \"null\" - simply omit it for the default behavior. Must be a valid directory path if provided.",
                ),
            ),
        ])
    })
}

/// Maps to CC `GlobTool.inputSchema`.
pub fn glob_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::GLOB_TOOL_NAME.to_string(),
        description: prompt::DESCRIPTION.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// Maps to: CC `tools/GlobTool/GlobTool.ts:55` `export type Output =
/// z.infer<OutputSchema>` (schema at :39-52) — the single type the tool
/// yields from `call()`, records as the message's `toolUseResult`, and the
/// render path recovers via `outputSchema.safeParse` ([`ui::parse_output`] is
/// the Rust stand-in). Numbers remain unrestricted Zod `number()` values so
/// cold JSONL keeps exact fidelity; live execution fills integral counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// CC `durationMs`.
    pub duration_ms: serde_json::Number,
    /// CC `numFiles`.
    pub num_files: serde_json::Number,
    /// CC `filenames`.
    pub filenames: Vec<String>,
    /// CC `truncated` — results limited to 100 files.
    pub truncated: bool,
}

fn requested_glob_root(
    args: &serde_json::Value,
    cwd: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    match args
        .get("path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
    {
        Some(path) => crate::utils::path::expand_path(path, Some(cwd)),
        None => Ok(cwd.to_path_buf()),
    }
}

fn to_relative_path(path: &std::path::Path, cwd: &std::path::Path) -> String {
    let Ok(relative) = path.strip_prefix(cwd) else {
        return path.display().to_string();
    };
    let relative = relative.display().to_string();
    if relative.starts_with("..") {
        path.display().to_string()
    } else {
        relative
    }
}

/// Execute a permitted Glob tool use, producing the CC-shaped output.
/// Maps to: CC `tools/GlobTool/GlobTool.ts:153-175` `call(...)`.
pub(crate) fn glob_output(
    args: &serde_json::Value,
    cwd: Option<&std::path::Path>,
) -> Result<Output, String> {
    glob_output_with_context(
        args,
        cwd,
        &crate::tool::AbortController::default(),
        &crate::tool::ToolPermissionContext::default(),
        100,
    )
}

fn glob_output_with_context(
    args: &serde_json::Value,
    cwd: Option<&std::path::Path>,
    abort_controller: &crate::tool::AbortController,
    permission_context: &crate::tool::ToolPermissionContext,
    max_results: usize,
) -> Result<Output, String> {
    let started = std::time::Instant::now();
    let pattern = args
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "missing pattern".to_string())?;
    let active_cwd = cwd
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(crate::bootstrap::state::get_original_cwd);
    let requested_root = requested_glob_root(args, &active_cwd)?;
    let result = crate::utils::glob::glob(
        pattern,
        &requested_root,
        max_results,
        0,
        abort_controller,
        permission_context,
    )?;
    let filenames = result
        .files
        .iter()
        .map(|path| to_relative_path(path, &active_cwd))
        .collect::<Vec<_>>();
    Ok(Output {
        duration_ms: serde_json::Number::from(started.elapsed().as_millis() as u64),
        num_files: serde_json::Number::from(filenames.len() as u64),
        filenames,
        truncated: result.truncated,
    })
}

/// Behavioral half of CC `GlobTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct GlobTool;

impl crate::tool::ToolCall for GlobTool {
    fn name(&self) -> &'static str {
        "Glob"
    }

    /// Maps to: CC `GlobTool.ts:143-145` `async prompt() { return
    /// DESCRIPTION }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::DESCRIPTION.to_string()
    }

    /// Maps to: CC `GlobTool.ts:61-63` `description()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::DESCRIPTION.to_string()
    }

    /// Maps to: CC `GlobTool.ts:64` mounting `UI.tsx:12-14` `userFacingName`
    /// — 'Search'.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "Search".to_string()
    }

    /// Maps to: CC `GlobTool.ts:151-153` `extractSearchText` — the filename
    /// list joined by newlines.
    fn extract_search_text(&self, data: &crate::tool::ToolOutput) -> Option<String> {
        match data {
            crate::tool::ToolOutput::Glob(output) => Some(output.filenames.join("\n")),
            _ => None,
        }
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("find files by name pattern or wildcard")
    }

    /// Maps to: CC `GlobTool.ts:19,65` mounting `UI.tsx#getToolUseSummary`.
    fn get_tool_use_summary(&self, args: &serde_json::Value) -> Option<String> {
        crate::tools::glob_tool::ui::get_tool_use_summary(Some(args))
    }

    /// Maps to CC `GlobTool.getActivityDescription(...)`.
    fn get_activity_description(&self, args: &serde_json::Value) -> Option<String> {
        Some(
            crate::tools::glob_tool::ui::get_tool_use_summary(Some(args))
                .map(|summary| format!("Finding {summary}"))
                .unwrap_or_else(|| "Finding files".to_string()),
        )
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `GlobTool.isConcurrencySafe(...)`.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `GlobTool.isSearchOrReadCommand(...)`.
    fn is_search_or_read_command(
        &self,
        _args: &serde_json::Value,
    ) -> Option<crate::tool::SearchOrReadCommand> {
        Some(crate::tool::SearchOrReadCommand {
            is_search: true,
            is_read: false,
        })
    }

    /// Maps to: CC `GlobTool.isReadOnly(...)`.
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    fn get_path(&self, args: &serde_json::Value) -> Option<String> {
        requested_glob_root(args, &crate::bootstrap::state::get_original_cwd())
            .ok()
            .map(|path| path.display().to_string())
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("pattern")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn prepare_permission_matcher(
        &self,
        args: &serde_json::Value,
    ) -> Option<crate::tool::PermissionPatternMatcher> {
        let pattern = args.get("pattern")?.as_str()?.to_string();
        Some(Box::new(move |rule_pattern| {
            crate::utils::permissions::shell_rule_matching::match_wildcard_pattern(
                rule_pattern,
                &pattern,
                false,
            )
        }))
    }

    /// Maps to: CC `tools/GlobTool/GlobTool.ts:96-132` `validateInput(...)`.
    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        let Some(path) = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
        else {
            return crate::tool::ValidationResult::Ok;
        };
        let cwd = context.effective_cwd();
        let absolute_path = match crate::utils::path::expand_path(path, Some(&cwd)) {
            Ok(path) => path,
            Err(message) => return crate::tool::ValidationResult::Fatal { message },
        };
        let absolute_text = absolute_path.display().to_string();
        // DEVIATION(SAFETY): also inspect the raw input so POSIX cannot turn a
        // Windows UNC spelling into a cwd-relative path before the intended CC
        // no-I/O credential-leak guard runs.
        if path.starts_with("\\\\")
            || path.starts_with("//")
            || absolute_text.starts_with("\\\\")
            || absolute_text.starts_with("//")
        {
            return crate::tool::ValidationResult::Ok;
        }
        match futures::executor::block_on(
            crate::utils::fs_operations::get_fs_implementation().stat(&absolute_path),
        ) {
            Ok(metadata) if metadata.is_dir() => crate::tool::ValidationResult::Ok,
            Ok(_) => crate::tool::ValidationResult::Error {
                message: format!("Path is not a directory: {path}"),
                error_code: 2,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut message = format!(
                    "Directory does not exist: {path}. {} {}.",
                    crate::utils::file::FILE_NOT_FOUND_CWD_NOTE,
                    cwd.display()
                );
                if let Some(suggestion) =
                    crate::utils::file::suggest_path_under_cwd(&absolute_path, &cwd)
                {
                    message.push_str(&format!(" Did you mean {}?", suggestion.display()));
                }
                crate::tool::ValidationResult::Error {
                    message,
                    error_code: 1,
                }
            }
            // CC rethrows non-ENOENT stat failures to the outer tool-call
            // error boundary rather than returning a semantic error code.
            Err(error) => crate::tool::ValidationResult::Fatal {
                message: error.to_string(),
            },
        }
    }

    /// Maps to: CC `GlobTool.checkPermissions(...)` through
    /// `checkReadPermissionForTool(...)`.
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let cwd = context.effective_cwd();
        let requested_root = requested_glob_root(args, &cwd).unwrap_or_else(|_| cwd.clone());
        let pattern = args
            .get("pattern")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let requested_permission =
            crate::utils::permissions::filesystem::check_read_permission_for_tool(
                &requested_root.display().to_string(),
                args,
                &context.tool_permission_context,
                &cwd,
            );
        // DEVIATION(SAFETY): CC getPath() ignores an absolute pattern even
        // though utils/glob.ts changes ripgrep's root to that pattern's base.
        // Evaluate both decisions before returning: a prompt for an unrelated
        // optional `path` must never mask a deny/prompt for the directory that
        // will actually be traversed.
        let permission_root =
            crate::utils::glob::effective_glob_search_root(pattern, &requested_root);
        if permission_root == requested_root {
            return requested_permission;
        }
        let traversal_permission =
            crate::utils::permissions::filesystem::check_read_permission_for_tool(
                &permission_root.display().to_string(),
                args,
                &context.tool_permission_context,
                &cwd,
            );
        use crate::utils::permissions::permission_result::PermissionResult;
        if matches!(&traversal_permission, PermissionResult::Deny { .. }) {
            return traversal_permission;
        }
        if matches!(&requested_permission, PermissionResult::Deny { .. }) {
            return requested_permission;
        }
        if !matches!(&traversal_permission, PermissionResult::Allow { .. }) {
            return traversal_permission;
        }
        requested_permission
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        let args = args.clone();
        let cwd = context.effective_cwd();
        let worker_abort = context.abort_controller.clone();
        let permission_context = context.tool_permission_context.clone();
        let max_results = context
            .glob_limits
            .as_ref()
            .and_then(|limits| limits.max_results)
            .unwrap_or(100);
        Box::pin(async move {
            let result = tokio::task::spawn_blocking(move || {
                glob_output_with_context(
                    &args,
                    Some(&cwd),
                    &worker_abort,
                    &permission_context,
                    max_results,
                )
            })
            .await
            .map_err(|error| format!("Glob worker failed: {error}"))
            .and_then(|result| result);
            match result {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Glob(output),
                    new_messages: Vec::new(),
                },
                Err(message) => crate::tool::ToolResult {
                    // The `Error: …` raw string rides the row via the
                    // `tool_use_result` trait projection below, not a display
                    // variant.
                    data: crate::tool::ToolOutput::Composed {
                        content: message.clone(),
                        status: crate::types::message::ToolResultStatus::Error,
                    },
                    new_messages: Vec::new(),
                },
            }
        })
    }

    /// Maps to: CC `tools/GlobTool/GlobTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:177) — 'No files found'
    /// (:182) or the filename list plus the truncation hint (:190-195).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::ToolResultStatus;
        match data {
            crate::tool::ToolOutput::Glob(output) => {
                let content = if output.filenames.is_empty() {
                    "No files found".to_string()
                } else {
                    let mut lines = output.filenames.clone();
                    if output.truncated {
                        lines.push(
                            "(Results are truncated. Consider using a more specific path or pattern.)"
                                .to_string(),
                        );
                    }
                    lines.join("\n")
                };
                (content, ToolResultStatus::Success)
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (String::new(), ToolResultStatus::Error),
        }
    }

    /// Glob carries no display shape — the raw the trait projects below
    /// is what renders (Glob reuses Grep's `renderToolResultMessage`,
    /// UI.tsx:56). A live `Output` is typed, so its projection always parses;
    /// the emit gate never fires here, unlike the cold paths that must gate
    /// on `ui::parse_output`.
    /// Maps to: CC recording GlobTool's `Output` as the message's
    /// `toolUseResult`. A failure records the `Error: …` string, matching the
    /// string `toolUseResult` CC keeps for errored searches.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::Glob(output) => {
                Some(crate::tools::glob_tool::ui::output_to_value(output))
            }
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => Some(serde_json::Value::String(format!("Error: {content}"))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{PinnedProjectDir, TEST_ENV_LOCK};

    #[test]
    fn relative_model_paths_keep_dotdot_prefixed_filenames_absolute() {
        let root = std::env::temp_dir().join(format!(
            "cometix-glob-relative-{}",
            uuid::Uuid::new_v4().simple()
        ));
        assert_eq!(
            to_relative_path(&root.join("normal.rs"), &root),
            "normal.rs"
        );
        assert_eq!(
            to_relative_path(&root.join("..notes"), &root),
            root.join("..notes").display().to_string()
        );
        assert_eq!(
            to_relative_path(&root.join("../outside.rs"), &root),
            root.join("../outside.rs").display().to_string()
        );
    }

    #[test]
    fn glob_tool_schema_matches_official_input_shape() {
        let schema = glob_tool_schema();
        assert_eq!(schema.name, "Glob");
        assert_eq!(schema.description, prompt::DESCRIPTION);
        assert_eq!(schema.input_schema["additionalProperties"], false);
        assert_eq!(
            schema.input_schema["required"],
            serde_json::json!(["pattern"])
        );
        for key in ["pattern", "path"] {
            assert!(
                schema
                    .input_schema
                    .pointer(&format!("/properties/{key}"))
                    .is_some()
            );
        }
    }

    #[test]
    fn glob_tool_metadata_matches_official_contract() {
        use crate::tool::ToolCall as _;

        let tool = GlobTool;
        let input = serde_json::json!({"pattern": "src/**/*.rs"});
        assert_eq!(
            tool.search_hint(),
            Some("find files by name pattern or wildcard")
        );
        assert_eq!(tool.max_result_size_chars(), 100_000);
        assert_eq!(
            tool.get_activity_description(&input),
            Some("Finding src/**/*.rs".to_string())
        );
        assert_eq!(
            tool.get_activity_description(&serde_json::json!({})),
            Some("Finding files".to_string())
        );
        assert!(tool.is_concurrency_safe(&input));
        assert!(tool.is_read_only(&input));
        assert_eq!(
            tool.is_search_or_read_command(&input),
            Some(crate::tool::SearchOrReadCommand {
                is_search: true,
                is_read: false,
            })
        );
        assert_eq!(tool.to_auto_classifier_input(&input), "src/**/*.rs");
        let matcher = tool
            .prepare_permission_matcher(&input)
            .expect("Glob exposes a hook matcher");
        assert!(matcher("src/**"));
        assert!(!matcher("tests/**"));

        let absolute = std::env::temp_dir().join("cometix-glob-get-path");
        assert_eq!(
            tool.get_path(&serde_json::json!({
                "pattern": "*",
                "path": absolute.display().to_string()
            })),
            Some(absolute.display().to_string())
        );
    }

    #[test]
    fn glob_validation_matches_official_directory_errors_and_unc_guard() {
        use crate::tool::ToolCall as _;

        let cwd = std::env::temp_dir().join(format!(
            "cometix-glob-validation-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let suggested_name = format!("suggested-dir-{}", uuid::Uuid::new_v4().simple());
        std::fs::create_dir_all(cwd.join(&suggested_name)).unwrap();
        std::fs::write(cwd.join("file.txt"), "x").unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(cwd.clone()),
            ..crate::tool::ToolUseContext::default()
        };

        assert_eq!(
            GlobTool.validate_input(&serde_json::json!({"pattern": "*"}), &context),
            crate::tool::ValidationResult::Ok
        );
        assert_eq!(
            GlobTool.validate_input(
                &serde_json::json!({"pattern": "*", "path": "file.txt"}),
                &context,
            ),
            crate::tool::ValidationResult::Error {
                message: "Path is not a directory: file.txt".to_string(),
                error_code: 2,
            }
        );
        assert_eq!(
            GlobTool.validate_input(
                &serde_json::json!({"pattern": "*", "path": "missing"}),
                &context,
            ),
            crate::tool::ValidationResult::Error {
                message: format!(
                    "Directory does not exist: missing. Note: your current working directory is {}.",
                    cwd.display()
                ),
                error_code: 1,
            }
        );
        assert_eq!(
            GlobTool.validate_input(
                &serde_json::json!({
                    "pattern": "*",
                    "path": cwd.parent().unwrap().join(&suggested_name).display().to_string()
                }),
                &context,
            ),
            crate::tool::ValidationResult::Error {
                message: format!(
                    "Directory does not exist: {}. Note: your current working directory is {}. Did you mean {}?",
                    cwd.parent().unwrap().join(&suggested_name).display(),
                    cwd.display(),
                    cwd.join(&suggested_name).display(),
                ),
                error_code: 1,
            }
        );
        assert_eq!(
            GlobTool.validate_input(
                &serde_json::json!({"pattern": "*", "path": "//server/share"}),
                &context,
            ),
            crate::tool::ValidationResult::Ok
        );
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn glob_permissions_authorize_cwd_and_ask_for_outside_or_absolute_pattern_roots() {
        use crate::tool::ToolCall as _;
        use crate::utils::permissions::permission_result::PermissionResult;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-glob-permission-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-glob-permission-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let traversal_outside = std::env::temp_dir().join(format!(
            "cometix-glob-permission-traversal-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&traversal_outside).unwrap();
        // CC filesystem.ts:667-674 authorizes original cwd, not every cwd override.
        let _project = PinnedProjectDir::at(&root);
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            ..crate::tool::ToolUseContext::default()
        };

        assert!(matches!(
            GlobTool.check_permissions(&serde_json::json!({"pattern": "**/*"}), &context),
            PermissionResult::Allow { .. }
        ));
        assert!(matches!(
            GlobTool.check_permissions(
                &serde_json::json!({
                    "pattern": "**/*",
                    "path": outside.display().to_string()
                }),
                &context,
            ),
            PermissionResult::Ask { .. }
        ));
        assert!(matches!(
            GlobTool.check_permissions(
                &serde_json::json!({
                    "pattern": format!("{}/*.txt", outside.display())
                }),
                &context,
            ),
            PermissionResult::Ask { .. }
        ));
        let traversal_ask = GlobTool.check_permissions(
            &serde_json::json!({
                "pattern": format!("{}/*.txt", traversal_outside.display()),
                "path": outside.display().to_string(),
            }),
            &context,
        );
        assert!(matches!(
            traversal_ask,
            PermissionResult::Ask { ref message, .. }
                if message.contains(&traversal_outside.display().to_string())
        ));

        std::fs::create_dir_all(root.join("blocked")).unwrap();
        let mut denied = context.clone();
        denied.tool_permission_context.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Read",
                Some("blocked/**".to_string()),
            )],
        );
        assert!(matches!(
            GlobTool.check_permissions(
                &serde_json::json!({"pattern": "*", "path": "blocked"}),
                &denied,
            ),
            PermissionResult::Deny { .. }
        ));
        denied
            .tool_permission_context
            .always_deny_rules
            .get_mut(&crate::types::permissions::PermissionRuleSource::Session)
            .unwrap()
            .push(crate::types::permissions::PermissionRuleValue::new(
                "Read",
                Some(format!("/{}/**", traversal_outside.display())),
            ));
        assert!(matches!(
            GlobTool.check_permissions(
                &serde_json::json!({
                    "pattern": format!("{}/*.txt", traversal_outside.display()),
                    "path": outside.display().to_string(),
                }),
                &denied,
            ),
            PermissionResult::Deny { .. }
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
        let _ = std::fs::remove_dir_all(traversal_outside);
    }

    #[cfg(unix)]
    #[test]
    fn glob_permission_resolves_explicit_directory_symlinks_before_allowing_read() {
        use crate::tool::ToolCall as _;
        use crate::utils::permissions::permission_result::PermissionResult;

        let root = std::env::temp_dir().join(format!(
            "cometix-glob-symlink-permission-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-glob-symlink-permission-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            ..crate::tool::ToolUseContext::default()
        };
        assert!(matches!(
            GlobTool.check_permissions(
                &serde_json::json!({"pattern": "*", "path": "linked"}),
                &context,
            ),
            PermissionResult::Ask { .. }
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn glob_read_deny_rules_hide_matching_files_before_results_are_returned() {
        let root = std::env::temp_dir().join(format!(
            "cometix-glob-deny-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::create_dir_all(root.join("rootless")).unwrap();
        std::fs::write(root.join("visible.txt"), "visible").unwrap();
        std::fs::write(root.join("secret/hidden.txt"), "hidden").unwrap();
        std::fs::write(root.join("rootless/hidden.txt"), "hidden").unwrap();
        let outside = std::env::temp_dir().join(format!(
            "cometix-glob-deny-target-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(outside.join("secret")).unwrap();
        std::fs::create_dir_all(outside.join("nested/secret")).unwrap();
        std::fs::write(outside.join("visible.txt"), "visible").unwrap();
        std::fs::write(outside.join("secret/hidden.txt"), "hidden").unwrap();
        std::fs::write(outside.join("nested/visible.txt"), "visible").unwrap();
        std::fs::write(outside.join("nested/secret/hidden.txt"), "hidden").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        let absolute_rule = format!("/{}", root.join("secret/**").display());
        let symlink_target_rule = format!("/{}", outside.join("secret/**").display());
        let nested_symlink_target_rule = format!("/{}", outside.join("nested/secret/**").display());
        let mut permissions = crate::tool::ToolPermissionContext::default();
        permissions.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![
                crate::types::permissions::PermissionRuleValue::new("Read", Some(absolute_rule)),
                crate::types::permissions::PermissionRuleValue::new(
                    "Read",
                    Some("rootless/**".to_string()),
                ),
                crate::types::permissions::PermissionRuleValue::new(
                    "Read",
                    Some(symlink_target_rule),
                ),
                crate::types::permissions::PermissionRuleValue::new(
                    "Read",
                    Some(nested_symlink_target_rule),
                ),
            ],
        );

        let output = glob_output_with_context(
            &serde_json::json!({"pattern": "**/*.txt"}),
            Some(&root),
            &crate::tool::AbortController::default(),
            &permissions,
            100,
        )
        .expect("glob succeeds");
        assert_eq!(output.filenames, vec!["visible.txt"]);

        let linked = glob_output_with_context(
            &serde_json::json!({"pattern": "**/*.txt", "path": "linked"}),
            Some(&root),
            &crate::tool::AbortController::default(),
            &permissions,
            100,
        )
        .expect("approved symlink-root glob succeeds");
        assert_eq!(
            linked
                .filenames
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            [
                "linked/visible.txt".to_string(),
                "linked/nested/visible.txt".to_string(),
            ]
            .into_iter()
            .collect()
        );
        assert!(!linked.filenames.iter().any(|path| path.contains("secret")));

        let nested = glob_output_with_context(
            &serde_json::json!({"pattern": "**/*.txt", "path": "linked/nested"}),
            Some(&root),
            &crate::tool::AbortController::default(),
            &permissions,
            100,
        )
        .expect("nested symlink-component glob succeeds");
        assert_eq!(nested.filenames, vec!["linked/nested/visible.txt"]);
        assert!(!nested.filenames.iter().any(|path| path.contains("secret")));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[tokio::test]
    async fn glob_call_honors_limits_usage_errors_and_abort_semantics() {
        use crate::tool::ToolCall as _;
        use crate::types::message::ToolResultStatus;
        use crate::types::permissions::PermissionMode;

        let root = std::env::temp_dir().join(format!(
            "cometix-glob-limit-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            glob_limits: Some(crate::tool::GlobLimits {
                max_results: Some(2),
            }),
            ..crate::tool::ToolUseContext::default()
        };
        let request = crate::utils::permissions::permissions::mock_permission_request(
            "permission-glob",
            "toolu-glob",
            "Glob",
            "*.txt",
            PermissionMode::Default,
        );
        let args = serde_json::json!({"pattern": "*.txt"});
        let result = GlobTool
            .call(&args, &request, &context, None, None, None)
            .await;
        assert!(matches!(
            result.data,
            crate::tool::ToolOutput::Glob(Output {
                ref num_files,
                truncated: true,
                ..
            }) if num_files == &serde_json::Number::from(2)
        ));

        let invalid_args = serde_json::json!({"pattern": "["});
        let invalid = GlobTool
            .call(&invalid_args, &request, &context, None, None, None)
            .await;
        assert!(matches!(
            invalid.data,
            crate::tool::ToolOutput::Glob(Output {
                ref num_files,
                truncated: false,
                ..
            }) if num_files == &serde_json::Number::from(0)
        ));

        let canceled_context = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            ..crate::tool::ToolUseContext::default()
        };
        canceled_context.abort_controller.abort();
        let canceled = GlobTool
            .call(&args, &request, &canceled_context, None, None, None)
            .await;
        assert!(matches!(
            canceled.data,
            crate::tool::ToolOutput::Composed {
                status: ToolResultStatus::Error,
                ref content,            } if content.starts_with("Ripgrep search timed out after ")
        ));
        // The `Error: …` raw string now rides the row via the trait.
        assert!(matches!(
            GlobTool.tool_use_result(&canceled.data),
            Some(serde_json::Value::String(ref raw))
                if raw.starts_with("Error: Ripgrep search timed out after ")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn glob_model_mapping_and_display_keep_official_empty_and_truncated_shapes() {
        use crate::tool::ToolCall as _;
        use crate::types::message::ToolResultStatus;

        let empty = crate::tool::ToolOutput::Glob(Output {
            duration_ms: serde_json::Number::from(3),
            num_files: serde_json::Number::from(0),
            filenames: Vec::new(),
            truncated: false,
        });
        assert_eq!(
            GlobTool.map_tool_result_to_tool_result_block_param(&empty, "toolu-empty"),
            ("No files found".to_string(), ToolResultStatus::Success)
        );

        let output = crate::tool::ToolOutput::Glob(Output {
            duration_ms: serde_json::Number::from(11),
            num_files: serde_json::Number::from(2),
            filenames: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            truncated: true,
        });
        assert_eq!(
            GlobTool.map_tool_result_to_tool_result_block_param(&output, "toolu-glob"),
            (
                "src/a.rs\nsrc/b.rs\n(Results are truncated. Consider using a more specific path or pattern.)"
                    .to_string(),
                ToolResultStatus::Success,
            )
        );
        // No Glob display shape — the trait projects the raw
        // `toolUseResult` with every output field intact.
        let raw = GlobTool
            .tool_use_result(&output)
            .expect("glob success projects raw output");
        assert_eq!(
            raw,
            serde_json::json!({
                "durationMs": 11,
                "numFiles": 2,
                "filenames": ["src/a.rs", "src/b.rs"],
                "truncated": true
            })
        );
        // The live projection always satisfies the tool's own output schema,
        // so the render-time `safeParse` gate never suppresses it.
        assert!(crate::tools::glob_tool::ui::parse_output(&raw).is_some());
    }

    #[test]
    fn glob_output_uses_official_result_cap_and_relative_paths() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-glob-cap-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..101 {
            std::fs::write(dir.join(format!("file-{index:03}.txt")), "x").unwrap();
        }

        let output = glob_output(&serde_json::json!({"pattern": "*.txt"}), Some(&dir))
            .expect("glob succeeds");
        assert_eq!(output.filenames.len(), 100);
        assert!(output.truncated);
        assert!(
            output
                .filenames
                .iter()
                .all(|path| !std::path::Path::new(path).is_absolute())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn glob_does_not_follow_directory_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "cometix-glob-symlink-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-glob-symlink-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();

        let output = glob_output(&serde_json::json!({"pattern": "**/*.txt"}), Some(&root))
            .expect("glob succeeds");
        assert!(output.filenames.is_empty());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn glob_output_uses_cwd_override_when_path_is_omitted() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-glob-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("needle.txt"), "match").unwrap();
        let args = serde_json::json!({"pattern": "*.txt"});

        let output = glob_output(&args, Some(&dir)).expect("glob succeeds");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            output
                .filenames
                .iter()
                .any(|path| path.ends_with("needle.txt"))
        );
    }
}
