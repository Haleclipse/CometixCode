//! Maps to:
//! - CC `tools/EnterWorktreeTool/EnterWorktreeTool.ts`
//! - CC `tools/EnterWorktreeTool/constants.ts`
//! - CC `tools/EnterWorktreeTool/prompt.ts`
//! - CC `tools/EnterWorktreeTool/UI.tsx`

pub mod prompt;
pub mod ui;

/// Maps to CC `EnterWorktreeTool.inputSchema`.
/// Maps to: CC `EnterWorktreeTool.ts:23-39` `inputSchema`.
///
/// The `superRefine` runs `validateWorktreeSlug` and reports the thrown
/// message verbatim, so the issue text is the validator's — same as CC.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::strict_object(vec![(
            "name",
            zod::string()
                .super_refine(|value| {
                    let slug = value.as_str()?;
                    crate::utils::worktree::validate_worktree_slug(slug)
                        .err()
                        .map(|message| zod::SuperRefineIssue {
                            message,
                            params: None,
                        })
                })
                .optional()
                .describe(
                    "Optional name for the worktree. Each \"/\"-separated segment may contain only letters, digits, dots, underscores, and dashes; max 64 chars total. A random name is generated if not provided.",
                ),
        )])
    })
}

pub fn enter_worktree_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::ENTER_WORKTREE_TOOL_NAME.to_string(),
        description: prompt::get_enter_worktree_tool_prompt().to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/EnterWorktreeTool/EnterWorktreeTool.ts` outputSchema (:42).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) worktree_path: String,
    pub(crate) worktree_branch: Option<String>,
    pub(crate) message: String,
}

/// Maps to: CC `EnterWorktreeTool.call` (:77-118).
pub(crate) async fn enter_worktree_output(input: &serde_json::Value) -> Result<Output, String> {
    use crate::utils::worktree::{create_worktree_for_session, get_current_worktree_session};

    if get_current_worktree_session().is_some() {
        return Err("Already in a worktree session".to_string());
    }

    // Resolve to main repo root so worktree creation works from within a worktree.
    if let Ok(cwd) = std::env::current_dir()
        && let Some(main_repo) = crate::utils::git::find_canonical_git_root(&cwd)
        && main_repo != cwd
    {
        let _ = std::env::set_current_dir(&main_repo);
        let _ = crate::utils::shell::set_cwd(&main_repo, None);
    }

    // CC `input.name ?? getPlanSlug()` (:90) — the schema's superRefine is
    // the only slug validation; call() takes the field verbatim (no trim,
    // no empty-string collapse, no re-validation).
    let slug = match input.get("name").and_then(|value| value.as_str()) {
        Some(name) => name.to_string(),
        None => {
            crate::utils::plans::get_plan_slug(Some(&crate::bootstrap::state::get_session_id()))
        }
    };

    let session =
        create_worktree_for_session(&crate::bootstrap::state::get_session_id(), &slug, None)
            .await
            .map_err(|e| e.to_string())?;

    // CC `process.chdir(worktreeSession.worktreePath)` (:94) — a failure
    // throws the runtime's own error message, so pass the OS error through
    // without invented framing.
    if let Err(err) = std::env::set_current_dir(&session.worktree_path) {
        return Err(err.to_string());
    }
    // CC feeds `setOriginalCwd(getCwd())`, i.e. the realpath'd value `setCwd`
    // resolved — not the join()'d `session.worktreePath` (ExitWorktreeTool.ts:246-249).
    let resolved_cwd =
        crate::utils::shell::set_cwd(std::path::Path::new(&session.worktree_path), None)
            .unwrap_or_else(|_| std::path::PathBuf::from(&session.worktree_path));
    crate::bootstrap::state::set_original_cwd(resolved_cwd);
    crate::utils::session_storage::save_worktree_state(
        crate::utils::worktree::worktree_session_to_persisted_json(&session),
    );
    // Clear cached system prompt sections so env_info_simple recomputes with
    // worktree context, plus the cwd-dependent memoized caches (CC :98-102).
    crate::constants::system_prompt_sections::clear_system_prompt_sections();
    crate::utils::claudemd::clear_memory_file_caches();
    crate::utils::plans::clear_plans_directory_cache();

    let branch_info = session
        .worktree_branch
        .as_ref()
        .map(|b| format!(" on branch {b}"))
        .unwrap_or_default();

    Ok(Output {
        worktree_path: session.worktree_path.clone(),
        worktree_branch: session.worktree_branch.clone(),
        message: format!(
            "Created worktree at {}{branch_info}. The session is now working in the worktree. Use ExitWorktree to leave mid-session, or exit the session to be prompted.",
            session.worktree_path
        ),
    })
}

/// Behavioral half of CC `EnterWorktreeTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct EnterWorktreeTool;

impl crate::tool::ToolCall for EnterWorktreeTool {
    fn name(&self) -> &'static str {
        "EnterWorktree"
    }

    /// Maps to: CC `EnterWorktreeTool.ts:59-61` `async prompt() { return
    /// getEnterWorktreeToolPrompt() }` — same source the wire schema renders.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_enter_worktree_tool_prompt().to_string()
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("create an isolated git worktree and switch into it")
    }

    /// Maps to: CC `EnterWorktreeTool.maxResultSizeChars` (:55).
    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `EnterWorktreeTool.userFacingName()` (:68-70).
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "Creating worktree".to_string()
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        _context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            match enter_worktree_output(args).await {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::EnterWorktree(output),
                    new_messages: Vec::new(),
                },
                Err(message) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Composed {
                        content: format!("Error: {message}"),
                        status: crate::types::message::ToolResultStatus::Error,
                    },
                    new_messages: Vec::new(),
                },
            }
        })
    }

    /// Maps to: CC `mapToolResultToToolResultBlockParam` (:120-126).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::EnterWorktree(output) => (
                output.message.clone(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>EnterWorktree returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording EnterWorktreeTool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::EnterWorktree(output) => Some(
                crate::tools::enter_worktree_tool::ui::output_to_value(output),
            ),
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
    #[test]
    fn enter_worktree_schema_matches_official_input_shape() {
        let schema = super::enter_worktree_tool_schema();
        assert_eq!(schema.name, "EnterWorktree");
        // `name` is optional, so zod emits no `required` at all.
        assert_eq!(schema.input_schema.get("required"), None);
        assert_eq!(
            schema.input_schema.pointer("/properties/name/type"),
            Some(&serde_json::json!("string"))
        );
        assert!(
            schema
                .description
                .contains("explicitly asks to work in a worktree")
        );
        assert_eq!(super::ui::render_tool_use_message(), "Creating worktree…");
    }
}
