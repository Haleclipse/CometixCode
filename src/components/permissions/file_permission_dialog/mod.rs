//! Maps to: CC `components/permissions/FilePermissionDialog/*`.
//!
//! This module ports the official file-permission dialog boundary shared by
//! filesystem, edit, write, and notebook permission requests. Runtime-only
//! slices such as IDE diff tabs, feedback text editing, and applying
//! `additionalWorkingDirectories` remain outside this UI component; callers
//! dispatch typed choices and the permission service applies updates.

pub mod ide_diff_config;
pub mod permission_options;
pub mod use_file_permission_dialog;
pub mod use_permission_handler;

pub use ide_diff_config::{
    FileEdit as IDEDiffFileEdit, IDEDiffChangeInput, IDEDiffConfig, IDEDiffEditMode,
    IDEDiffSupport, create_single_edit_diff_config,
};

use self::permission_options::{
    FilePermissionOptionsParams, PermissionSessionScope, get_file_permission_options_from_params,
};
use super::permission_dialog::PermissionDialog;
use super::worker_badge::WorkerBadgeProps;
use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionMode, PermissionPromptChoice, PermissionPromptResponse, PermissionUpdate,
    PermissionUpdateDestination,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileOperationType {
    Read,
    #[default]
    Write,
    Create,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePermissionOptionValue {
    Yes,
    YesSession,
    YesClaudeFolder,
    YesGlobalClaudeFolder,
    No,
}

impl FilePermissionOptionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::YesSession => "yes-session",
            Self::YesClaudeFolder => "yes-claude-folder",
            Self::YesGlobalClaudeFolder => "yes-global-claude-folder",
            Self::No => "no",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePermissionOption {
    pub label: String,
    pub value: FilePermissionOptionValue,
}

impl FilePermissionOption {
    fn select(label: impl Into<String>, value: FilePermissionOptionValue) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }

    pub fn to_select_option(&self) -> SelectOptionData {
        SelectOptionData {
            label: self.label.clone(),
            description: None,
            dim_description: true,
            value: self.value.as_str().to_string(),
            disabled: false,
            input: None,
        }
    }
}

#[derive(Default, Props)]
pub struct FilePermissionDialogProps {
    pub title: String,
    pub subtitle: Option<String>,
    pub question: Option<String>,
    /// Text convenience for official `content?: React.ReactNode`.
    pub content: Option<String>,
    /// Rich retained content (StructuredDiff/HighlightedCode/etc.).
    pub content_children: Vec<AnyElement<'static>>,
    pub path: String,
    pub operation_type: FileOperationType,
    pub tool_permission_context: Option<ToolPermissionContext>,
    pub worker_badge: Option<WorkerBadgeProps>,
    pub on_select: Handler<FilePermissionOptionValue>,
    pub on_cancel: Handler<()>,
}

/// Maps to: CC `FilePermissionDialog/permissionOptions.tsx`
/// `getFilePermissionOptions(...)`.
pub fn get_file_permission_options(
    file_path: &str,
    operation_type: FileOperationType,
) -> Vec<FilePermissionOption> {
    get_file_permission_options_with_context(
        file_path,
        operation_type,
        &ToolPermissionContext::default(),
    )
}

/// Maps to: CC `FilePermissionDialog/permissionOptions.tsx`
/// `getFilePermissionOptions(...)` with the live `ToolPermissionContext` used
/// by `pathInAllowedWorkingPath`.
pub fn get_file_permission_options_with_context(
    file_path: &str,
    operation_type: FileOperationType,
    tool_permission_context: &ToolPermissionContext,
) -> Vec<FilePermissionOption> {
    let params = FilePermissionOptionsParams {
        file_path: file_path.to_string(),
        operation_type,
        in_allowed_working_path: path_in_allowed_working_path_with_context(
            file_path,
            tool_permission_context,
        ),
        in_claude_folder: is_in_claude_folder(file_path),
        in_global_claude_folder: is_in_global_claude_folder(file_path),
        directory_name: directory_name_for_path(file_path),
        mode_cycle_shortcut: "shift+tab".to_string(),
        yes_input_mode: false,
        no_input_mode: false,
    };

    get_file_permission_options_from_params(&params)
        .into_iter()
        .map(|option| {
            let value = match option.value.as_str() {
                "yes" => FilePermissionOptionValue::Yes,
                "yes-session" => FilePermissionOptionValue::YesSession,
                "yes-claude-folder" => FilePermissionOptionValue::YesClaudeFolder,
                "yes-global-claude-folder" => FilePermissionOptionValue::YesGlobalClaudeFolder,
                "no" => FilePermissionOptionValue::No,
                _ => FilePermissionOptionValue::No,
            };
            FilePermissionOption::select(option.label, value)
        })
        .collect()
}

/// Maps to: CC `FilePermissionDialog/usePermissionHandler.ts`
/// `PERMISSION_HANDLERS` option branches, projected through the existing Rust
/// prompt-choice seam.
pub fn file_permission_option_to_prompt_choice(
    value: FilePermissionOptionValue,
) -> PermissionPromptChoice {
    match value {
        FilePermissionOptionValue::Yes => PermissionPromptChoice::AllowOnce,
        FilePermissionOptionValue::YesSession
        | FilePermissionOptionValue::YesClaudeFolder
        | FilePermissionOptionValue::YesGlobalClaudeFolder => PermissionPromptChoice::AlwaysAllow,
        FilePermissionOptionValue::No => PermissionPromptChoice::Deny,
    }
}

/// Maps to: CC `FilePermissionDialog/usePermissionHandler.ts` calling
/// `ToolUseConfirm.onAllow(toolUseConfirm.input, suggestions)` for file dialog
/// options. The `permission_updates_explicit` marker preserves the official
/// distinction between an explicit empty suggestions list and legacy
/// `AlwaysAllow` choices that add `request.rule`.
pub fn file_permission_option_to_prompt_response(
    value: FilePermissionOptionValue,
    file_path: &str,
    operation_type: FileOperationType,
    tool_permission_context: &ToolPermissionContext,
) -> PermissionPromptResponse {
    let choice = file_permission_option_to_prompt_choice(value);
    match value {
        FilePermissionOptionValue::Yes => PermissionPromptResponse::new(choice),
        FilePermissionOptionValue::YesSession => PermissionPromptResponse::new(choice)
            .with_permission_updates(generate_file_permission_suggestions(
                file_path,
                operation_type,
                tool_permission_context,
            )),
        FilePermissionOptionValue::YesClaudeFolder => PermissionPromptResponse::new(choice)
            .with_permission_updates(vec![
                use_permission_handler::claude_folder_permission_update(
                    PermissionSessionScope::ClaudeFolder,
                ),
            ]),
        FilePermissionOptionValue::YesGlobalClaudeFolder => PermissionPromptResponse::new(choice)
            .with_permission_updates(vec![
                use_permission_handler::claude_folder_permission_update(
                    PermissionSessionScope::GlobalClaudeFolder,
                ),
            ]),
        FilePermissionOptionValue::No => PermissionPromptResponse::new(choice),
    }
}

/// Rust/iocraft enum adapter delegating to CC
/// `utils/permissions/filesystem.ts:1414-1474#generateSuggestions`'s canonical
/// owner in `utils::permissions::filesystem`.
pub fn generate_file_permission_suggestions(
    file_path: &str,
    operation_type: FileOperationType,
    tool_permission_context: &ToolPermissionContext,
) -> Vec<PermissionUpdate> {
    let operation_type = match operation_type {
        FileOperationType::Read => {
            crate::utils::permissions::filesystem::FilesystemOperationType::Read
        }
        FileOperationType::Write => {
            crate::utils::permissions::filesystem::FilesystemOperationType::Write
        }
        FileOperationType::Create => {
            crate::utils::permissions::filesystem::FilesystemOperationType::Create
        }
    };
    crate::utils::permissions::filesystem::generate_suggestions(
        file_path,
        operation_type,
        tool_permission_context,
        None,
    )
}

/// Maps to: CC `FilePermissionDialog/permissionOptions.tsx` `isInClaudeFolder`.
pub fn is_in_claude_folder(file_path: &str) -> bool {
    let path = normalize_absolute_path(file_path);
    let claude_dir = normalize_path(&crate::bootstrap::state::get_original_cwd().join(".claude"));
    is_descendant_of(&path, &claude_dir)
}

/// Maps to: CC `FilePermissionDialog/permissionOptions.tsx`
/// `isInGlobalClaudeFolder`.
pub fn is_in_global_claude_folder(file_path: &str) -> bool {
    let Some(home) = home_dir() else {
        return false;
    };
    let path = normalize_absolute_path(file_path);
    let claude_dir = normalize_path(&home.join(".claude"));
    is_descendant_of(&path, &claude_dir)
}

/// Maps to: CC `utils/permissions/filesystem.ts` `pathInAllowedWorkingPath`.
pub fn path_in_allowed_working_path(file_path: &str) -> bool {
    path_in_allowed_working_path_with_context(file_path, &ToolPermissionContext::default())
}

/// Maps to: CC `utils/permissions/filesystem.ts` `pathInAllowedWorkingPath`
/// with `ToolPermissionContext.additionalWorkingDirectories` included.
pub fn path_in_allowed_working_path_with_context(
    file_path: &str,
    tool_permission_context: &ToolPermissionContext,
) -> bool {
    crate::utils::permissions::filesystem::path_in_allowed_working_path(
        file_path,
        tool_permission_context,
        None,
    )
}

pub fn file_permission_relative_to_cwd(file_path: &str) -> String {
    let path = PathBuf::from(file_path);
    let cwd = crate::bootstrap::state::get_original_cwd();
    let from = cwd.components().collect::<Vec<_>>();
    let to = path.components().collect::<Vec<_>>();
    if from.first() != to.first() {
        return file_path.to_string();
    }
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..from.len() {
        relative.push("..");
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    relative.display().to_string()
}

pub fn file_permission_basename(file_path: &str) -> String {
    Path::new(file_path)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(file_path)
        .to_string()
}

/// Maps to: CC
/// `FilesystemPermissionRequest.tsx:11-21#pathFromToolUse` — `'getPath' in tool
/// && typeof tool.getPath === 'function'` then `tool.getPath(input)` inside a
/// `try`/`catch` that returns `null`.
///
/// The Rust analogue of `toolUseConfirm.tool` is the runtime registry
/// (`find_tool_call`), and `ToolCall::get_path` is the ported `Tool.getPath?`
/// (default `None` = CC's absent method). This used to be a hardcoded
/// tool-name→input-key table, which had already drifted: it returned `None` for
/// a `Glob`/`Grep` without an explicit `path`, while `GlobTool.ts:88-90` /
/// `GrepTool.ts:195-197` fall back to `getCwd()`, so those requests took CC's
/// "no path → FallbackPermissionRequest" branch by accident.
pub fn file_permission_path_from_input(
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<String> {
    crate::services::tools::tool_execution::find_tool_call(tool_name)
        .and_then(|tool| tool.get_path(input))
        .filter(|path| !path.is_empty())
}

/// Maps to: CC `FilesystemPermissionRequest.tsx:33-35`
/// `toolUseConfirm.tool.userFacingName(toolUseConfirm.input)`.
///
/// Resolved off the registry so each tool answers for itself, exactly as CC
/// does. The previous hardcoded table returned `"Edit"` for FileEditTool —
/// a string `UI.tsx:28-52` never produces (it returns `Update`, `Create` or
/// `Updated plan`) — plus `"Write"` for a plan-directory write, `"Glob"` and
/// `"Grep"` where both CC tools return `"Search"`.
///
/// An unresolved name falls back to the tool name, which is `buildTool`'s
/// EFFECTIVE default: `TOOL_DEFAULTS.userFacingName` is `() => ''`, but
/// `buildTool` overwrites it with `() => def.name` before spreading `def`
/// (`Tool.ts:786-791`).
pub fn file_permission_user_facing_name(tool_name: &str, input: &serde_json::Value) -> String {
    crate::services::tools::tool_execution::find_tool_call(tool_name)
        .map(|tool| tool.user_facing_name(Some(input)))
        .unwrap_or_else(|| tool_name.to_string())
}

/// Maps to: CC `FilesystemPermissionRequest.tsx:65-68`
/// `toolUseConfirm.tool.renderToolUseMessage(input, { theme, verbose })`.
///
/// `verbose` is the component's `verbose` prop, not a constant — the Fallback
/// dialog is the one that hardcodes `verbose: true`
/// (`FallbackPermissionRequest.tsx:168-171`).
pub fn file_permission_tool_use_message(
    tool_name: &str,
    input: &serde_json::Value,
    verbose: bool,
) -> String {
    crate::components::messages::assistant_tool_use_message::render_tool_use_message(
        tool_name,
        input,
        crate::components::messages::user_tool_result_message::utils::ToolRenderOptions {
            verbose,
            ..Default::default()
        },
    )
    .unwrap_or_default()
}

pub fn symlink_target_for_dialog(
    file_path: &str,
    operation_type: FileOperationType,
) -> Option<String> {
    if operation_type == FileOperationType::Read {
        return None;
    }
    let path = expand_path(file_path);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_symlink() {
        return None;
    }
    path.canonicalize()
        .ok()
        .map(|path| path.display().to_string())
}

fn symlink_warning(file_path: &str, operation_type: FileOperationType) -> Option<String> {
    let target = symlink_target_for_dialog(file_path, operation_type)?;
    let outside_cwd = std::env::current_dir()
        .ok()
        .map(|cwd| !normalize_path(Path::new(&target)).starts_with(normalize_path(&cwd)))
        .unwrap_or(false);
    Some(if outside_cwd {
        format!("This will modify {target} (outside working directory) via a symlink")
    } else {
        format!("Symlink target: {target}")
    })
}

/// Maps to: CC `FilePermissionDialog/FilePermissionDialog.tsx` render path.
#[component]
pub fn FilePermissionDialog(
    props: &mut FilePermissionDialogProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let tool_permission_context = props.tool_permission_context.clone().unwrap_or_default();
    let options = get_file_permission_options_with_context(
        &props.path,
        props.operation_type,
        &tool_permission_context,
    );
    let option_count = options.len().max(1);
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_select = hooks.use_state(|| Option::<FilePermissionOptionValue>::None);
    let runtime = hooks
        .try_use_context::<crate::keybindings::keybinding_context::KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    let select_handlers: crate::keybindings::use_keybinding::KeybindingHandlers = vec![
        (
            "select:previous".to_string(),
            Box::new(move || {
                focused_index.set(focused_index.get().saturating_sub(1));
                true
            }),
        ),
        ("select:next".to_string(), {
            let option_count = option_count;
            Box::new(move || {
                focused_index.set((focused_index.get() + 1).min(option_count - 1));
                true
            })
        }),
        ("select:accept".to_string(), {
            let options = options.clone();
            Box::new(move || {
                if let Some(option) = options.get(focused_index.get()) {
                    pending_select.set(Some(option.value));
                }
                true
            })
        }),
        (
            "select:cancel".to_string(),
            Box::new(move || {
                // Maps to: CC Select.onCancel -> reject option.
                pending_select.set(Some(FilePermissionOptionValue::No));
                true
            }),
        ),
    ];
    crate::keybindings::use_keybinding::use_keybindings(
        &mut hooks,
        runtime.clone(),
        select_handlers,
        crate::keybindings::types::ContextName::Select,
        || true,
    );
    crate::keybindings::use_keybinding::use_keybinding(
        &mut hooks,
        runtime,
        "confirm:cycleMode",
        crate::keybindings::types::ContextName::Confirmation,
        || true,
        {
            let options = options.clone();
            move || {
                if options
                    .iter()
                    .any(|option| option.value == FilePermissionOptionValue::YesSession)
                {
                    pending_select.set(Some(FilePermissionOptionValue::YesSession));
                }
                true
            }
        },
    );

    let selected = { pending_select.read().clone() };
    if let Some(value) = selected {
        pending_select.set(None);
        (props.on_select)(value);
    }
    let focused = focused_index.get().min(option_count - 1);
    let select_options = options
        .iter()
        .map(FilePermissionOption::to_select_option)
        .collect::<Vec<_>>();
    let question = props
        .question
        .clone()
        .unwrap_or_else(|| "Do you want to proceed?".to_string());
    let warning = symlink_warning(&props.path, props.operation_type);
    let content_children = props.content_children.drain(..).collect::<Vec<_>>();

    element! {
        View(flex_direction: FlexDirection::Column) {
            PermissionDialog(
                title: props.title.clone(),
                subtitle: props.subtitle.clone(),
                inner_padding_x: Some(0u32),
                worker_badge: props.worker_badge.clone(),
            ) {
                #(warning.map(|warning| element! {
                    View(padding_left: 1u32, padding_right: 1u32, margin_bottom: 1u32) {
                        Text(content: warning, color: theme.warning, wrap: TextWrap::Wrap)
                    }
                }))
                #(if !content_children.is_empty() {
                    Some(element! {
                        View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
                            #(content_children)
                        }
                    }.into_any())
                } else {
                    props.content.as_ref().filter(|content| !content.trim().is_empty()).map(|content| element! {
                        View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                            Text(content: content.clone(), wrap: TextWrap::Wrap)
                        }
                    }.into_any())
                })
                View(flex_direction: FlexDirection::Column, padding_left: 1u32, padding_right: 1u32) {
                    Text(content: question, wrap: TextWrap::Wrap)
                    Select(
                        options: select_options,
                        focused_index: focused,
                        visible_option_count: option_count,
                        layout: SelectLayout::Compact,
                        hide_indexes: true,
                    )
                }
            }
            View(padding_left: 1u32, padding_right: 1u32, margin_top: 1u32) {
                Text(content: "Esc to cancel · Tab to amend".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
            }
        }
    }
}

fn directory_name_for_path(file_path: &str) -> String {
    Path::new(&get_directory_for_path(file_path))
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("this directory")
        .to_string()
}

fn get_directory_for_path(file_path: &str) -> String {
    let path = expand_path(file_path);
    if path.is_dir() {
        return path.display().to_string();
    }
    path.parent()
        .map(|parent| parent.display().to_string())
        .filter(|parent| !parent.is_empty())
        .unwrap_or_else(|| ".".to_string())
}

fn home_dir() -> Option<PathBuf> {
    crate::utils::process_env::var_os("HOME")
        .or_else(|| crate::utils::process_env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn expand_path(file_path: &str) -> PathBuf {
    let path = PathBuf::from(file_path);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn normalize_absolute_path(file_path: &str) -> PathBuf {
    normalize_path(&expand_path(file_path))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn is_descendant_of(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent) && path != parent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::PermissionBehavior;
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn render_dialog(path: String, operation_type: FileOperationType) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                FilePermissionDialog(
                    title: "Edit file".to_string(),
                    subtitle: Some(file_permission_relative_to_cwd(&path)),
                    question: Some(format!("Do you want to edit {}?", file_permission_basename(&path))),
                    content: Some(format!("Edit({path})")),
                    path: path,
                    operation_type: operation_type,
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn file_permission_options_match_official_read_write_and_claude_folder_labels() {
        // Fixtures are real in-repo paths; the project dir must agree.
        let _project_dir = crate::utils::env_utils::PinnedProjectDir::at_manifest_root();
        let cwd = std::env::current_dir().unwrap();
        let inside = cwd.join("src/main.rs");
        let options =
            get_file_permission_options(&inside.display().to_string(), FileOperationType::Write);
        assert_eq!(options[0].label, "Yes");
        assert_eq!(
            options[1].label,
            "Yes, allow all edits during this session (shift+tab)"
        );
        assert_eq!(options[2].label, "No");

        let read_options =
            get_file_permission_options(&inside.display().to_string(), FileOperationType::Read);
        assert_eq!(read_options[1].label, "Yes, during this session");

        let claude_path = crate::bootstrap::state::get_original_cwd()
            .join(".claude")
            .join("settings.json");
        let claude_options = get_file_permission_options(
            &claude_path.display().to_string(),
            FileOperationType::Write,
        );
        assert_eq!(
            claude_options[1].label,
            "Yes, and allow Claude to edit its own settings for this session"
        );
    }

    #[test]
    fn file_permission_options_label_outside_directory_like_official() {
        let path = std::env::temp_dir()
            .join("cometix-file-perm")
            .join("example.txt");
        let options =
            get_file_permission_options(&path.display().to_string(), FileOperationType::Read);
        assert!(
            options[1]
                .label
                .contains("Yes, allow reading from cometix-file-perm/ during this session"),
            "label={}",
            options[1].label
        );
    }

    #[test]
    fn file_permission_options_honor_additional_working_directories_context() {
        let outside = std::env::temp_dir()
            .join("cometix-file-perm-context")
            .join("example.txt");
        let mut context = ToolPermissionContext::default();
        context.additional_working_directories.insert(
            std::env::temp_dir()
                .join("cometix-file-perm-context")
                .display()
                .to_string(),
            crate::types::permissions::AdditionalWorkingDirectory {
                path: std::env::temp_dir()
                    .join("cometix-file-perm-context")
                    .display()
                    .to_string(),
                source: crate::types::permissions::PermissionRuleSource::Session,
            },
        );

        let options = get_file_permission_options_with_context(
            &outside.display().to_string(),
            FileOperationType::Read,
            &context,
        );
        assert_eq!(options[1].label, "Yes, during this session");
    }

    #[test]
    fn file_permission_suggestions_match_official_session_updates() {
        // Fixtures are real in-repo paths; the project dir must agree.
        let _project_dir = crate::utils::env_utils::PinnedProjectDir::at_manifest_root();
        let cwd_file = std::env::current_dir().unwrap().join("src/main.rs");
        let updates = generate_file_permission_suggestions(
            &cwd_file.display().to_string(),
            FileOperationType::Write,
            &ToolPermissionContext::default(),
        );
        assert_eq!(
            updates,
            vec![PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::AcceptEdits,
            }]
        );

        let accept_edits = ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        };
        let response = file_permission_option_to_prompt_response(
            FilePermissionOptionValue::YesSession,
            &cwd_file.display().to_string(),
            FileOperationType::Write,
            &accept_edits,
        );
        assert_eq!(response.choice, PermissionPromptChoice::AlwaysAllow);
        assert!(response.permission_updates.is_empty());
        assert!(response.permission_updates_explicit);

        let outside = std::env::temp_dir()
            .join("cometix-file-perm-suggestions")
            .join("example.txt");
        let outside_updates = generate_file_permission_suggestions(
            &outside.display().to_string(),
            FileOperationType::Create,
            &ToolPermissionContext::default(),
        );
        assert!(matches!(
            outside_updates.as_slice(),
            [
                PermissionUpdate::SetMode {
                    mode: PermissionMode::AcceptEdits,
                    ..
                },
                PermissionUpdate::AddDirectories { .. }
            ]
        ));

        let read_updates = generate_file_permission_suggestions(
            &outside.display().to_string(),
            FileOperationType::Read,
            &ToolPermissionContext::default(),
        );
        assert!(matches!(
            read_updates.first(),
            Some(PermissionUpdate::AddRules {
                behavior: PermissionBehavior::Allow,
                rules,
                ..
            }) if rules.first().is_some_and(|rule| rule.tool_name == "Read" && rule.rule_content.as_deref().is_some_and(|content| content.ends_with("/**")))
        ));
    }

    #[test]
    fn file_permission_dialog_renders_official_shell_and_footer() {
        // Fixtures are real in-repo paths; the project dir must agree.
        let _project_dir = crate::utils::env_utils::PinnedProjectDir::at_manifest_root();
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let text = render_dialog(path.display().to_string(), FileOperationType::Write);
        assert!(text.contains("Edit file"), "canvas=\n{text}");
        assert!(
            text.contains("Do you want to edit main.rs?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Yes, allow all edits during this session"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Esc to cancel"), "canvas=\n{text}");
    }

    #[tokio::test]
    async fn file_permission_dialog_enter_and_escape_dispatch_choices() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let selected = Arc::new(Mutex::new(Vec::new()));
        let cancelled = Arc::new(Mutex::new(0usize));
        let selected_clone = selected.clone();
        let cancelled_clone = cancelled.clone();
        let mut app = element! {
            ContextProvider(value: Context::owned(
                crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
            )) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    FilePermissionDialog(
                    title: "Edit file".to_string(),
                    path: path.display().to_string(),
                    operation_type: FileOperationType::Write,
                    on_select: move |value| selected_clone.lock().unwrap().push(value),
                    on_cancel: move |_| *cancelled_clone.lock().unwrap() += 1,
                    )
                }
            }
        };
        let mut render_loop = Box::pin(
            app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(vec![key(KeyCode::Esc)]))
                    .with_size(120, 30),
            ),
        );
        for _ in 0..10 {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(100)).await;
                None
            })
            .await;
            if next.is_none() {
                break;
            }
        }
        assert_eq!(
            selected.lock().unwrap().as_slice(),
            &[FilePermissionOptionValue::No]
        );
        assert_eq!(*cancelled.lock().unwrap(), 0);
    }
}
