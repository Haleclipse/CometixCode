//! Maps to:
//! - CC `tools/ExitWorktreeTool/ExitWorktreeTool.ts`
//! - CC `tools/ExitWorktreeTool/constants.ts`
//! - CC `tools/ExitWorktreeTool/prompt.ts`
//! - CC `tools/ExitWorktreeTool/UI.tsx`

pub mod prompt;
pub mod ui;

/// Maps to CC `ExitWorktreeTool.inputSchema`.
/// Maps to: CC `ExitWorktreeTool.ts:30-44` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "action",
                zod::enumeration(vec!["keep", "remove"]).describe(
                    "\"keep\" leaves the worktree and branch on disk; \"remove\" deletes both.",
                ),
            ),
            (
                "discard_changes",
                zod::boolean().optional().describe(
                    "Required true when action is \"remove\" and the worktree has uncommitted files or unmerged commits. The tool will refuse and list them otherwise.",
                ),
            ),
        ])
    })
}

pub fn exit_worktree_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::EXIT_WORKTREE_TOOL_NAME.to_string(),
        description: prompt::get_exit_worktree_tool_prompt().to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/ExitWorktreeTool/ExitWorktreeTool.ts` outputSchema (:47).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) action: String,
    pub(crate) original_cwd: String,
    pub(crate) worktree_path: String,
    pub(crate) worktree_branch: Option<String>,
    pub(crate) tmux_session_name: Option<String>,
    pub(crate) discarded_files: Option<u64>,
    pub(crate) discarded_commits: Option<u64>,
    pub(crate) message: String,
}

/// Maps to: CC `ExitWorktreeTool.validateInput` (:174-223).
pub(crate) fn validate_exit_worktree_input(
    input: &serde_json::Value,
) -> crate::tool::ValidationResult {
    use crate::utils::worktree::{count_worktree_changes, get_current_worktree_session};

    let Some(session) = get_current_worktree_session() else {
        return crate::tool::ValidationResult::error(
            "No-op: there is no active EnterWorktree session to exit. This tool only operates on worktrees created by EnterWorktree in the current session — it will not touch worktrees created manually or in a previous session. No filesystem changes were made.",
            1,
        );
    };

    let action = input
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("keep");
    let discard_changes = input
        .get("discard_changes")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if action == "remove" && !discard_changes {
        match count_worktree_changes(
            &session.worktree_path,
            session.original_head_commit.as_deref(),
        ) {
            None => {
                return crate::tool::ValidationResult::error(
                    format!(
                        "Could not verify worktree state at {}. Refusing to remove without explicit confirmation. Re-invoke with discard_changes: true to proceed — or use action: \"keep\" to preserve the worktree.",
                        session.worktree_path
                    ),
                    3,
                );
            }
            Some(summary) if summary.changed_files > 0 || summary.commits > 0 => {
                let mut parts = Vec::new();
                if summary.changed_files > 0 {
                    parts.push(format!(
                        "{} uncommitted {}",
                        summary.changed_files,
                        if summary.changed_files == 1 {
                            "file"
                        } else {
                            "files"
                        }
                    ));
                }
                if summary.commits > 0 {
                    let branch = session
                        .worktree_branch
                        .as_deref()
                        .unwrap_or("the worktree branch");
                    parts.push(format!(
                        "{} {} on {branch}",
                        summary.commits,
                        if summary.commits == 1 {
                            "commit"
                        } else {
                            "commits"
                        }
                    ));
                }
                return crate::tool::ValidationResult::error(
                    format!(
                        "Worktree has {}. Removing will discard this work permanently. Confirm with the user, then re-invoke with discard_changes: true — or use action: \"keep\" to preserve the worktree.",
                        parts.join(" and ")
                    ),
                    2,
                );
            }
            Some(_) => {}
        }
    }

    crate::tool::ValidationResult::Ok
}

/// Maps to: CC `ExitWorktreeTool.call` (:227-320) + `restoreSessionToOriginalCwd`.
pub(crate) fn exit_worktree_output(input: &serde_json::Value) -> Result<Output, String> {
    use crate::utils::worktree::{
        cleanup_worktree, count_worktree_changes, get_current_worktree_session, keep_worktree,
        kill_tmux_session,
    };

    let session =
        get_current_worktree_session().ok_or_else(|| "Not in a worktree session".to_string())?;

    let action = input
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("keep")
        .to_string();
    let original_cwd = session.original_cwd.clone();
    let worktree_path = session.worktree_path.clone();
    let worktree_branch = session.worktree_branch.clone();
    let tmux_session_name = session.tmux_session_name.clone();
    let original_head_commit = session.original_head_commit.clone();

    let summary = count_worktree_changes(&worktree_path, original_head_commit.as_deref())
        .unwrap_or(crate::utils::worktree::WorktreeChangeSummary {
            changed_files: 0,
            commits: 0,
        });

    if action == "keep" {
        keep_worktree();
        restore_session_to_original_cwd(&original_cwd);

        let tmux_note = tmux_session_name
            .as_ref()
            .map(|name| {
                format!(
                    " Tmux session {name} is still running; reattach with: tmux attach -t {name}"
                )
            })
            .unwrap_or_default();
        let branch_note = worktree_branch
            .as_ref()
            .map(|b| format!(" on branch {b}"))
            .unwrap_or_default();

        return Ok(Output {
            action,
            original_cwd: original_cwd.clone(),
            worktree_path: worktree_path.clone(),
            worktree_branch,
            tmux_session_name,
            discarded_files: None,
            discarded_commits: None,
            message: format!(
                "Exited worktree. Your work is preserved at {worktree_path}{branch_note}. Session is now back in {original_cwd}.{tmux_note}"
            ),
        });
    }

    // action === 'remove'
    if let Some(ref name) = tmux_session_name {
        let _ = kill_tmux_session(name);
    }
    cleanup_worktree();
    restore_session_to_original_cwd(&original_cwd);

    let mut discard_parts = Vec::new();
    if summary.commits > 0 {
        discard_parts.push(format!(
            "{} {}",
            summary.commits,
            if summary.commits == 1 {
                "commit"
            } else {
                "commits"
            }
        ));
    }
    if summary.changed_files > 0 {
        discard_parts.push(format!(
            "{} uncommitted {}",
            summary.changed_files,
            if summary.changed_files == 1 {
                "file"
            } else {
                "files"
            }
        ));
    }
    let discard_note = if discard_parts.is_empty() {
        String::new()
    } else {
        format!(" Discarded {}.", discard_parts.join(" and "))
    };

    Ok(Output {
        action,
        original_cwd: original_cwd.clone(),
        worktree_path: worktree_path.clone(),
        worktree_branch,
        tmux_session_name: None,
        discarded_files: Some(summary.changed_files),
        discarded_commits: Some(summary.commits),
        message: format!(
            "Exited and removed worktree at {worktree_path}.{discard_note} Session is now back in {original_cwd}."
        ),
    })
}

/// Maps to: CC `restoreSessionToOriginalCwd` (:122-146) — the session-layer
/// inverse of `EnterWorktreeTool.call`.
///
/// TODO(parity): CC also restores `setProjectRoot` + `updateHooksConfigSnapshot`
/// when `getProjectRoot() === getOriginalCwd()` (:135-141). That branch only
/// fires for the `--worktree` startup flag, which owns the sole
/// `setProjectRoot` write; neither the flag nor a project-root state slot exists
/// here, so the predicate is structurally false and the branch has nothing to
/// undo.
fn restore_session_to_original_cwd(original_cwd: &str) {
    // CC's comment on this helper: "keepWorktree()/cleanupWorktree() handle
    // process.chdir" — the utility layer owns the process chdir; this layer
    // only updates shell/session state (setCwd resolves, never chdirs).
    let resolved = crate::utils::shell::set_cwd(std::path::Path::new(original_cwd), None)
        .unwrap_or_else(|_| std::path::PathBuf::from(original_cwd));
    // EnterWorktree points originalCwd at the worktree (intentional — see
    // state.ts getProjectRoot comment). Reset to the real original.
    crate::bootstrap::state::set_original_cwd(resolved);
    crate::utils::session_storage::save_worktree_state(serde_json::Value::Null);
    crate::constants::system_prompt_sections::clear_system_prompt_sections();
    crate::utils::claudemd::clear_memory_file_caches();
    crate::utils::plans::clear_plans_directory_cache();
}

/// Behavioral half of CC `ExitWorktreeTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct ExitWorktreeTool;

impl crate::tool::ToolCall for ExitWorktreeTool {
    fn name(&self) -> &'static str {
        "ExitWorktree"
    }

    /// Maps to: CC `ExitWorktreeTool.ts:155-157` `async prompt() { return
    /// getExitWorktreeToolPrompt() }` — same source the wire schema renders.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_exit_worktree_tool_prompt().to_string()
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("exit a worktree session and return to the original directory")
    }

    /// Maps to: CC `ExitWorktreeTool.maxResultSizeChars` (:151).
    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `ExitWorktreeTool.userFacingName()` (:164-166).
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "Exiting worktree".to_string()
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn is_destructive(&self, args: &serde_json::Value) -> bool {
        args.get("action").and_then(|v| v.as_str()) == Some("remove")
    }

    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        validate_exit_worktree_input(args)
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
            match exit_worktree_output(args) {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::ExitWorktree(output),
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

    /// Maps to: CC `mapToolResultToToolResultBlockParam` (:322-328).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::ExitWorktree(output) => (
                output.message.clone(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>ExitWorktree returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording ExitWorktreeTool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::ExitWorktree(output) => Some(
                crate::tools::exit_worktree_tool::ui::output_to_value(output),
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
    fn exit_worktree_schema_matches_official_input_shape() {
        let schema = super::exit_worktree_tool_schema();
        assert_eq!(schema.name, "ExitWorktree");
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!(["action"]))
        );
        assert_eq!(
            schema.input_schema.pointer("/properties/action/enum"),
            Some(&serde_json::json!(["keep", "remove"]))
        );
        assert!(schema.description.contains("created by EnterWorktree"));
        assert_eq!(super::ui::render_tool_use_message(), "Exiting worktree…");
    }

    #[test]
    fn validate_input_fails_closed_without_session() {
        // Ensure no session from other tests.
        crate::utils::worktree::restore_worktree_session(None);
        let result = super::validate_exit_worktree_input(&serde_json::json!({
            "action": "keep"
        }));
        match result {
            crate::tool::ValidationResult::Error { error_code, .. } => {
                assert_eq!(error_code, 1);
            }
            crate::tool::ValidationResult::Ok => panic!("expected error without session"),
            crate::tool::ValidationResult::Fatal { message } => {
                panic!("unexpected fatal validation: {message}")
            }
        }
    }
}
