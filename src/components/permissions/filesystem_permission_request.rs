//! Maps to: CC
//! `components/permissions/FilesystemPermissionRequest/FilesystemPermissionRequest.tsx`.
//!
//! Shared permission request for read-only filesystem tools (`Read`, `Glob`,
//! `Grep`) and any filesystem tool that exposes a path but does not have a
//! specialized edit/write dialog.

use super::file_permission_dialog::{
    FileOperationType, FilePermissionDialog, FilePermissionOptionValue,
    file_permission_option_to_prompt_response, file_permission_path_from_input,
    file_permission_tool_use_message, file_permission_user_facing_name,
};
use super::worker_badge::WorkerBadgeProps;
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionPromptChoice, PermissionPromptResponse, PermissionRequest as PermissionRequestData,
};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct FilesystemPermissionRequestProps {
    pub request: Option<PermissionRequestData>,
    pub tool_permission_context: Option<ToolPermissionContext>,
    pub worker_badge: Option<WorkerBadgeProps>,
    /// Maps to: CC `PermissionRequestProps.verbose`
    /// (`PermissionRequest.tsx:121`), forwarded into
    /// `tool.renderToolUseMessage(input, { theme, verbose })` at `:65-68`.
    ///
    /// The producer chain is CC's: `useAppState(s => s.verbose)`
    /// (`REPL.tsx:973`) → `<PermissionRequest verbose={verbose} …>` (`:6045`) →
    /// `PermissionRequest.tsx:227`. It was a named seam until #134 wired that
    /// chain; `FileReadTool/UI.tsx:53-62` gates the ` · lines N-M` suffix on it.
    pub verbose: bool,
    pub on_select: Handler<PermissionPromptChoice>,
    pub on_select_response: Handler<PermissionPromptResponse>,
    pub on_cancel: Handler<()>,
}

fn default_request() -> PermissionRequestData {
    PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: "Read".to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: crate::types::permissions::PermissionRuleValue::new("Read", None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: crate::types::permissions::PermissionMode::Default,
    }
}

/// Maps to: CC `FilesystemPermissionRequest.tsx:32` `pathFromToolUse(...)`,
/// whose `null` selects the `FallbackPermissionRequest` branch at `:47-58`.
///
/// The `input_summary` fallback this used to carry has no CC counterpart, and
/// after #123 `input_summary` is the permission-RULE-content projection rather
/// than a display string — substituting it here hid the missing path instead of
/// taking CC's fallback branch.
pub fn filesystem_permission_path(request: &PermissionRequestData) -> Option<String> {
    file_permission_path_from_input(&request.tool_name, &request.input)
}

/// Maps to: CC `FilesystemPermissionRequest.tsx:61-72` — the `content` box,
/// `` `${userFacingName}(${tool.renderToolUseMessage(input, {theme, verbose})})` ``.
///
/// CC always emits the parentheses; a renderer returning `null` contributes no
/// children, so the line is `Name()`. That is the same shape
/// `fallback_tool_preview` produces for `FallbackPermissionRequest.tsx:166-172`.
pub fn filesystem_permission_content(request: &PermissionRequestData, verbose: bool) -> String {
    let user_facing_name = file_permission_user_facing_name(&request.tool_name, &request.input);
    let message = file_permission_tool_use_message(&request.tool_name, &request.input, verbose);
    format!("{user_facing_name}({message})")
}

/// Maps to: CC `FilesystemPermissionRequest.tsx:37-41` —
/// `` `${toolUseConfirm.tool.isReadOnly(input) ? 'Read' : 'Edit'} file` ``.
pub fn filesystem_permission_title(request: &PermissionRequestData) -> String {
    let is_read_only = crate::services::tools::tool_execution::find_tool_call(&request.tool_name)
        .is_some_and(|tool| tool.is_read_only(&request.input));
    let operation = if is_read_only { "Read" } else { "Edit" };
    format!("{operation} file")
}

/// Maps to: CC `FilesystemPermissionRequest` render path.
#[component]
pub fn FilesystemPermissionRequest(
    props: &mut FilesystemPermissionRequestProps,
    _hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let request = props.request.clone().unwrap_or_else(default_request);
    let title = filesystem_permission_title(&request);
    let content = filesystem_permission_content(&request, props.verbose);
    // Maps to: CC `FilesystemPermissionRequest.tsx:47-58` — no path means the
    // generic dialog, not an empty-path file dialog.
    let Some(path) = filesystem_permission_path(&request) else {
        let on_select = props.on_select.clone();
        let on_select_response = props.on_select_response.clone();
        return element! {
            super::fallback_permission_request::FallbackPermissionRequest(
                request: Some(request),
                worker_badge: props.worker_badge.clone(),
                on_select: Handler::from(move |response: PermissionPromptResponse| {
                    (on_select)(response.choice);
                    (on_select_response)(response);
                }),
            )
        }
        .into_any();
    };
    // Maps to: CC `:85` `operationType={isReadOnly ? 'read' : 'write'}`, the
    // same `tool.isReadOnly(input)` the title uses.
    let operation_type =
        if crate::services::tools::tool_execution::find_tool_call(&request.tool_name)
            .is_some_and(|tool| tool.is_read_only(&request.input))
        {
            FileOperationType::Read
        } else {
            FileOperationType::Write
        };

    let tool_permission_context =
        props
            .tool_permission_context
            .clone()
            .unwrap_or_else(|| ToolPermissionContext {
                mode: request.mode,
                ..ToolPermissionContext::default()
            });
    let path_for_response = path.clone();
    let context_for_response = tool_permission_context.clone();
    let on_select = props.on_select.clone();
    let on_select_response = props.on_select_response.clone();
    let on_cancel = props.on_cancel.clone();

    element! {
        FilePermissionDialog(
            title: title,
            content: Some(content),
            path: path,
            operation_type: operation_type,
            tool_permission_context: Some(tool_permission_context),
            worker_badge: props.worker_badge.clone(),
            on_select: move |value: FilePermissionOptionValue| {
                let response = file_permission_option_to_prompt_response(
                    value,
                    &path_for_response,
                    operation_type,
                    &context_for_response,
                );
                (on_select)(response.choice);
                (on_select_response)(response);
            },
            on_cancel: move |_| {
                (on_cancel)(());
            },
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionMode, PermissionRuleValue};
    use crate::utils::theme;

    fn read_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "Read".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: "/repo/src/main.rs".to_string(),
            input: serde_json::json!({ "file_path": "/repo/src/main.rs", "offset": 10, "limit": 3 }),
            call_input: None,
            rule: PermissionRuleValue::new("Read", Some("/repo/src/main.rs".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn request_for(tool_name: &str, input: serde_json::Value) -> PermissionRequestData {
        let mut request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                "perm-fs",
                "toolu_fs",
                tool_name,
                serde_json::to_string(&input).unwrap_or_default(),
                input,
                PermissionMode::Default,
            );
        request.rule.tool_name = tool_name.to_string();
        request
    }

    #[test]
    fn filesystem_permission_request_helpers_match_official_copy() {
        let request = read_request();
        assert_eq!(
            filesystem_permission_path(&request).as_deref(),
            Some("/repo/src/main.rs")
        );
        assert_eq!(filesystem_permission_title(&request), "Read file");
        // Re-derived 2026-08-26 (#132/#128): this line used to assert
        // `Read(/repo/src/main.rs · lines 10-12`, which is the VERBOSE
        // rendering. `FileReadTool/UI.tsx:59` gates the line range on
        // `verbose && (offset || limit)`, and
        // `FilesystemPermissionRequest.tsx:65-68` passes the component's
        // `verbose` PROP — only `FallbackPermissionRequest.tsx:168-171`
        // hardcodes `verbose: true`. The port hardcoded `true` here, and the
        // test transcribed that. Evidence pointed at the port, so the
        // expectation moved, not CC.
        assert_eq!(
            filesystem_permission_content(&request, false),
            "Read(/repo/src/main.rs)"
        );
        assert_eq!(
            filesystem_permission_content(&request, true),
            "Read(/repo/src/main.rs · lines 10-12)"
        );
    }

    /// Maps to: CC `FilesystemPermissionRequest.tsx:33-35`
    /// `toolUseConfirm.tool.userFacingName(toolUseConfirm.input)`, resolved per
    /// tool rather than from a name table:
    /// - `FileEditTool/UI.tsx:28-52` — `Updated plan` (plans dir) / `Create`
    ///   (`old_string === ''`) / `Update`. It never returns `"Edit"`.
    /// - `FileWriteTool/UI.tsx:86-93` — `Updated plan` / `Write`.
    /// - `GlobTool/UI.tsx:12-14` and `GrepTool.ts:169-171` — both `Search`.
    #[test]
    fn filesystem_permission_user_facing_name_matches_official_tool_owned_names() {
        let plans = crate::utils::plans::get_plans_directory();
        let plan_file = plans.join("2026-08-26-plan.md").display().to_string();

        let update = request_for(
            "Edit",
            serde_json::json!({
                "file_path": "/repo/src/main.rs",
                "old_string": "old",
                "new_string": "new",
            }),
        );
        assert_eq!(
            file_permission_user_facing_name(&update.tool_name, &update.input),
            "Update"
        );

        let create = request_for(
            "Edit",
            serde_json::json!({
                "file_path": "/repo/src/main.rs",
                "old_string": "",
                "new_string": "new",
            }),
        );
        assert_eq!(
            file_permission_user_facing_name(&create.tool_name, &create.input),
            "Create"
        );

        let plan_edit = request_for(
            "Edit",
            serde_json::json!({
                "file_path": plan_file,
                "old_string": "old",
                "new_string": "new",
            }),
        );
        assert_eq!(
            file_permission_user_facing_name(&plan_edit.tool_name, &plan_edit.input),
            "Updated plan"
        );

        let plan_write = request_for(
            "Write",
            serde_json::json!({ "file_path": plan_file, "content": "plan" }),
        );
        assert_eq!(
            file_permission_user_facing_name(&plan_write.tool_name, &plan_write.input),
            "Updated plan"
        );
        let write = request_for(
            "Write",
            serde_json::json!({ "file_path": "/repo/src/main.rs", "content": "x" }),
        );
        assert_eq!(
            file_permission_user_facing_name(&write.tool_name, &write.input),
            "Write"
        );

        let glob = request_for("Glob", serde_json::json!({ "pattern": "**/*.rs" }));
        assert_eq!(
            file_permission_user_facing_name(&glob.tool_name, &glob.input),
            "Search"
        );
        let grep = request_for("Grep", serde_json::json!({ "pattern": "fn main" }));
        assert_eq!(
            file_permission_user_facing_name(&grep.tool_name, &grep.input),
            "Search"
        );
    }

    /// Maps to: CC `GlobTool.ts:88-90` `getPath({ path }) { return path ?
    /// expandPath(path) : getCwd() }` and `GrepTool.ts:195-197`
    /// `getPath({ path }) { return path || getCwd() }`, reached through
    /// `FilesystemPermissionRequest.tsx:11-21#pathFromToolUse`.
    ///
    /// The removed name→key table returned `None` here, which took CC's
    /// "no path" branch (`:47-58`, the generic Fallback dialog) for every
    /// pathless Glob/Grep.
    #[test]
    fn filesystem_permission_path_matches_official_get_path_cwd_default() {
        let cwd = crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string();
        let grep = request_for("Grep", serde_json::json!({ "pattern": "fn main" }));
        assert_eq!(filesystem_permission_path(&grep).as_deref(), Some(&*cwd));
        let glob = request_for("Glob", serde_json::json!({ "pattern": "**/*.rs" }));
        assert_eq!(filesystem_permission_path(&glob).as_deref(), Some(&*cwd));
    }

    /// Maps to: CC `FilesystemPermissionRequest.tsx:61-72` — the rendered
    /// content line is `` `${userFacingName}(${renderToolUseMessage(...)})` ``,
    /// so an ordinary Edit shows `Update(...)`, an `old_string: ''` Edit shows
    /// `Create(...)` (`FileEditTool/UI.tsx:44-50`), and a plans-directory path
    /// shows `Updated plan()` — `UI.tsx:74-81` returns `''` for plan files
    /// because "path is already in userFacingName".
    ///
    /// None of the three could be produced by the name table this replaced: it
    /// answered `"Edit"` for all Edit inputs and `"Write"` for all writes.
    #[test]
    fn filesystem_permission_content_matches_official_edit_create_and_plan_names() {
        let update = request_for(
            "Edit",
            serde_json::json!({
                "file_path": "/repo/src/main.rs",
                "old_string": "old",
                "new_string": "new",
            }),
        );
        assert_eq!(
            filesystem_permission_content(&update, false),
            "Update(/repo/src/main.rs)"
        );

        let create = request_for(
            "Edit",
            serde_json::json!({
                "file_path": "/repo/src/main.rs",
                "old_string": "",
                "new_string": "new",
            }),
        );
        assert_eq!(
            filesystem_permission_content(&create, false),
            "Create(/repo/src/main.rs)"
        );

        let plan_file = crate::utils::plans::get_plans_directory()
            .join("2026-08-26-plan.md")
            .display()
            .to_string();
        let plan_edit = request_for(
            "Edit",
            serde_json::json!({
                "file_path": plan_file,
                "old_string": "old",
                "new_string": "new",
            }),
        );
        assert_eq!(
            filesystem_permission_content(&plan_edit, false),
            "Updated plan()"
        );

        // The pre-fix table could only ever emit these two.
        for content in [
            filesystem_permission_content(&update, false),
            filesystem_permission_content(&create, false),
            filesystem_permission_content(&plan_edit, false),
        ] {
            assert!(!content.starts_with("Edit("), "content={content}");
        }
    }

    #[test]
    fn filesystem_permission_request_renders_file_dialog() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                FilesystemPermissionRequest(request: Some(read_request()))
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Read file"), "canvas=\n{text}");
        assert!(text.contains("Read(/repo/src/main.rs"), "canvas=\n{text}");
        assert!(text.contains("Do you want to proceed?"), "canvas=\n{text}");
    }

    /// Maps to: CC `FilesystemPermissionRequest.tsx:61-72` passing the
    /// component's `verbose` PROP into
    /// `toolUseConfirm.tool.renderToolUseMessage(input, { theme, verbose })`
    /// (`:65-68`), and `FileReadTool/UI.tsx:53-62`, which appends
    /// `` ` · lines ${offset}-${offset + limit - 1}` `` only when
    /// `verbose && (offset || limit)`.
    ///
    /// The prop's producer is the REPL: `useAppState(s => s.verbose)`
    /// (`REPL.tsx:973`) → `<PermissionRequest verbose={verbose} …>` (`:6045`) →
    /// `PermissionRequest.tsx:227`.
    #[test]
    fn filesystem_permission_request_matches_official_verbose_line_range_gate() {
        let render = |verbose: bool| {
            element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    FilesystemPermissionRequest(request: Some(read_request()), verbose: verbose)
                }
            }
            .render(Some(120))
            .to_string()
        };

        let quiet = render(false);
        assert!(
            quiet.contains("Read(/repo/src/main.rs)"),
            "canvas=\n{quiet}"
        );
        assert!(
            !quiet.contains("lines 10-12"),
            "the line range is verbose-only; canvas=\n{quiet}"
        );

        let verbose = render(true);
        assert!(
            verbose.contains("Read(/repo/src/main.rs · lines 10-12)"),
            "canvas=\n{verbose}"
        );
    }
}
