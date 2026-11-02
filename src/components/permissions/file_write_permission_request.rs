//! Maps to: CC
//! `components/permissions/FileWritePermissionRequest/FileWritePermissionRequest.tsx`.
//!
//! Ports the write/create-specific permission dialog copy, file-existence
//! branch, and retained `FileWriteToolDiff` display boundary. IDE edits remain
//! an explicit `IDEDiffConfig`/apply callback owned by the dialog runtime.

use super::file_permission_dialog::{
    FileOperationType, FilePermissionDialog, FilePermissionOptionValue, IDEDiffConfig,
    IDEDiffFileEdit, create_single_edit_diff_config, file_permission_basename,
    file_permission_option_to_prompt_response, file_permission_relative_to_cwd,
};
use super::file_write_tool_diff::FileWriteToolDiff;
use super::worker_badge::WorkerBadgeProps;
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionPromptChoice, PermissionPromptResponse, PermissionRequest as PermissionRequestData,
};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct FileWritePermissionRequestProps {
    pub request: Option<PermissionRequestData>,
    pub tool_permission_context: Option<ToolPermissionContext>,
    pub worker_badge: Option<WorkerBadgeProps>,
    pub on_select: Handler<PermissionPromptChoice>,
    pub on_select_response: Handler<PermissionPromptResponse>,
    pub on_cancel: Handler<()>,
}

fn default_request() -> PermissionRequestData {
    PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: "Write".to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: crate::types::permissions::PermissionRuleValue::new("Write", None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: crate::types::permissions::PermissionMode::Default,
    }
}

pub fn file_write_permission_path(input: &serde_json::Value) -> String {
    input
        .get("file_path")
        .or_else(|| input.get("filePath"))
        .or_else(|| input.get("path"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn file_write_permission_content(input: &serde_json::Value) -> String {
    input
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Maps to: CC `FileWritePermissionRequest` single `readFileSync(file_path)`
/// memo that drives both create/overwrite copy and `FileWriteToolDiff`.
pub fn file_write_permission_existing_content(path: &str) -> Option<String> {
    if path.trim().is_empty() {
        return None;
    }
    match crate::utils::file_read::read_file_sync(std::path::Path::new(path)) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("failed to read Write permission preview for {path}: {error}"),
    }
}

pub fn file_write_permission_file_exists(path: &str) -> bool {
    file_write_permission_existing_content(path).is_some()
}

/// Maps to: CC `FileWritePermissionRequest.tsx#ideDiffSupport.getConfig`.
pub fn file_write_ide_diff_config(input: &serde_json::Value) -> IDEDiffConfig {
    let path = file_write_permission_path(input);
    let old_content = file_write_permission_existing_content(&path).unwrap_or_default();
    create_single_edit_diff_config(
        path,
        old_content,
        file_write_permission_content(input),
        Some(false),
    )
}

/// Maps to: CC `FileWritePermissionRequest.tsx#ideDiffSupport.applyChanges`.
pub fn apply_file_write_ide_changes(
    input: &serde_json::Value,
    modified_edits: &[IDEDiffFileEdit],
) -> serde_json::Value {
    let mut updated = input.clone();
    let Some(first_edit) = modified_edits.first() else {
        return updated;
    };
    let object = updated
        .as_object_mut()
        .expect("file write permission input is an object");
    object.insert(
        "content".to_string(),
        serde_json::Value::String(first_edit.new_string.clone()),
    );
    updated
}

/// Maps to: CC `FileWritePermissionRequest` render path.
#[component]
pub fn FileWritePermissionRequest(
    props: &mut FileWritePermissionRequestProps,
    _hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let request = props.request.clone().unwrap_or_else(default_request);
    let path = file_write_permission_path(&request.input);
    let existing_content = file_write_permission_existing_content(&path);
    let exists = existing_content.is_some();
    let old_content = existing_content.unwrap_or_default();
    let title = if exists {
        "Overwrite file"
    } else {
        "Create file"
    };
    let action_text = if exists { "overwrite" } else { "create" };
    let basename = file_permission_basename(&path);
    let subtitle = (!path.trim().is_empty()).then(|| file_permission_relative_to_cwd(&path));
    let question = format!("Do you want to {action_text} {basename}?");
    let new_content = file_write_permission_content(&request.input);

    let operation_type = if exists {
        FileOperationType::Write
    } else {
        FileOperationType::Create
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
            title: title.to_string(),
            subtitle: subtitle,
            question: Some(question),
            content_children: vec![element! {
                FileWriteToolDiff(
                    file_path: path.clone(),
                    content: new_content,
                    file_exists: exists,
                    old_content: old_content,
                )
            }.into_any()],
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionMode, PermissionRuleValue};
    use crate::utils::theme;

    fn write_request(path: String) -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "Write".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: path.clone(),
            input: serde_json::json!({
                "file_path": path,
                "content": "line one\nline two"
            }),
            call_input: None,
            rule: PermissionRuleValue::new("Write", Some("file".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    #[test]
    fn file_write_permission_helpers_match_official_create_overwrite_branch() {
        let temp_path =
            std::env::temp_dir().join(format!("nyan-write-perm-{}", std::process::id()));
        let _ = std::fs::remove_file(&temp_path);
        assert!(!file_write_permission_file_exists(
            &temp_path.display().to_string()
        ));
        std::fs::write(&temp_path, "old").unwrap();
        assert!(file_write_permission_file_exists(
            &temp_path.display().to_string()
        ));
        let _ = std::fs::remove_file(&temp_path);
    }

    #[test]
    fn file_write_ide_diff_support_matches_official_get_config_and_apply_changes() {
        let path =
            std::env::temp_dir().join(format!("nyan-write-perm-ide-diff-{}", std::process::id()));
        std::fs::write(&path, "old content").unwrap();
        let input = serde_json::json!({
            "file_path": path.display().to_string(),
            "content": "new content"
        });
        let config = file_write_ide_diff_config(&input);
        let _ = std::fs::remove_file(&path);
        assert_eq!(config.file_path, path.display().to_string());
        assert_eq!(config.edits[0].old_string, "old content");
        assert_eq!(config.edits[0].new_string, "new content");
        assert_eq!(config.edits[0].replace_all, Some(false));

        let updated = apply_file_write_ide_changes(
            &input,
            &[super::IDEDiffFileEdit {
                old_string: "old content".to_string(),
                new_string: "modified content".to_string(),
                replace_all: Some(false),
            }],
        );
        assert_eq!(updated["content"], "modified content");
    }

    #[test]
    fn file_write_permission_request_renders_create_copy() {
        let path =
            std::env::temp_dir().join(format!("nyan-write-perm-render-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // The dialog renders FileWriteToolDiff, which reads
        // `settings.syntax_highlighting_disabled` from AppState. Defaults are
        // the fixture: this asserts the create-path copy, not highlighting.
        let path_for_tree = path.display().to_string();
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        FileWritePermissionRequest(request: Some(write_request(path_for_tree.clone())))
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        assert!(text.contains("Create file"), "canvas=\n{text}");
        assert!(text.contains("Do you want to create"), "canvas=\n{text}");
        assert!(text.contains("line one"), "canvas=\n{text}");
    }

    #[test]
    fn file_write_permission_request_renders_overwrite_diff_boundary() {
        let path =
            std::env::temp_dir().join(format!("nyan-write-perm-overwrite-{}", std::process::id()));
        std::fs::write(&path, "old line\nline two\n").unwrap();
        // Same read as the create case; the assertion here is on the diff
        // markers, which the default (highlighting enabled) still emits.
        let path_for_tree = path.display().to_string();
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        FileWritePermissionRequest(request: Some(write_request(path_for_tree.clone())))
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();
        let _ = std::fs::remove_file(&path);

        assert!(text.contains("Overwrite file"), "canvas=\n{text}");
        assert!(text.contains("Do you want to overwrite"), "canvas=\n{text}");
        assert!(text.contains("-old line"), "canvas=\n{text}");
        assert!(text.contains("+line one"), "canvas=\n{text}");
    }
}
