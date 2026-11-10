//! Maps to: CC `components/permissions/FilePermissionDialog/permissionOptions.tsx`.
//!
//! Pure option-shaping helpers for file permission dialogs. The top-level
//! `file_permission_dialog::get_file_permission_options(...)` keeps the current
//! environment-backed convenience wrapper; this module mirrors the official
//! parameterized helper shape so callers/tests can exercise input-mode and
//! working-directory branches without touching global state.

use super::FileOperationType;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionSessionScope {
    ClaudeFolder,
    GlobalClaudeFolder,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionOption {
    AcceptOnce,
    AcceptSession {
        scope: Option<PermissionSessionScope>,
    },
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionOptionWithLabel {
    pub value: String,
    pub label: String,
    pub option: PermissionOption,
    pub input_placeholder: Option<String>,
    pub allow_empty_submit_to_cancel: bool,
}

impl PermissionOptionWithLabel {
    fn select(value: &str, label: impl Into<String>, option: PermissionOption) -> Self {
        Self {
            value: value.to_string(),
            label: label.into(),
            option,
            input_placeholder: None,
            allow_empty_submit_to_cancel: false,
        }
    }

    fn input(value: &str, label: &str, placeholder: &str, option: PermissionOption) -> Self {
        Self {
            value: value.to_string(),
            label: label.to_string(),
            option,
            input_placeholder: Some(placeholder.to_string()),
            allow_empty_submit_to_cancel: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePermissionOptionsParams {
    pub file_path: String,
    pub operation_type: FileOperationType,
    pub in_allowed_working_path: bool,
    pub in_claude_folder: bool,
    pub in_global_claude_folder: bool,
    pub directory_name: String,
    pub mode_cycle_shortcut: String,
    pub yes_input_mode: bool,
    pub no_input_mode: bool,
}

impl Default for FilePermissionOptionsParams {
    fn default() -> Self {
        Self {
            file_path: String::new(),
            operation_type: FileOperationType::Write,
            in_allowed_working_path: true,
            in_claude_folder: false,
            in_global_claude_folder: false,
            directory_name: "this directory".to_string(),
            mode_cycle_shortcut: "shift+tab".to_string(),
            yes_input_mode: false,
            no_input_mode: false,
        }
    }
}

/// Maps to: CC `permissionOptions.tsx#getFilePermissionOptions`.
pub fn get_file_permission_options_from_params(
    params: &FilePermissionOptionsParams,
) -> Vec<PermissionOptionWithLabel> {
    let mut options = Vec::new();

    if params.yes_input_mode {
        options.push(PermissionOptionWithLabel::input(
            "yes",
            "Yes",
            "and tell nyan what to do next",
            PermissionOption::AcceptOnce,
        ));
    } else {
        options.push(PermissionOptionWithLabel::select(
            "yes",
            "Yes",
            PermissionOption::AcceptOnce,
        ));
    }

    if (params.in_claude_folder || params.in_global_claude_folder)
        && params.operation_type != FileOperationType::Read
    {
        options.push(PermissionOptionWithLabel::select(
            if params.in_global_claude_folder {
                "yes-global-claude-folder"
            } else {
                "yes-claude-folder"
            },
            "Yes, and allow nyan to edit its own settings for this session",
            PermissionOption::AcceptSession {
                scope: Some(if params.in_global_claude_folder {
                    PermissionSessionScope::GlobalClaudeFolder
                } else {
                    PermissionSessionScope::ClaudeFolder
                }),
            },
        ));
    } else {
        let label = if params.in_allowed_working_path {
            match params.operation_type {
                FileOperationType::Read => "Yes, during this session".to_string(),
                FileOperationType::Write | FileOperationType::Create => format!(
                    "Yes, allow all edits during this session ({})",
                    params.mode_cycle_shortcut
                ),
            }
        } else {
            match params.operation_type {
                FileOperationType::Read => format!(
                    "Yes, allow reading from {}/ during this session",
                    params.directory_name
                ),
                FileOperationType::Write | FileOperationType::Create => format!(
                    "Yes, allow all edits in {}/ during this session ({})",
                    params.directory_name, params.mode_cycle_shortcut
                ),
            }
        };
        options.push(PermissionOptionWithLabel::select(
            "yes-session",
            label,
            PermissionOption::AcceptSession { scope: None },
        ));
    }

    if params.no_input_mode {
        options.push(PermissionOptionWithLabel::input(
            "no",
            "No",
            "and tell nyan what to do differently",
            PermissionOption::Reject,
        ));
    } else {
        options.push(PermissionOptionWithLabel::select(
            "no",
            "No",
            PermissionOption::Reject,
        ));
    }

    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_options_match_official_input_mode_and_session_labels() {
        let options = get_file_permission_options_from_params(&FilePermissionOptionsParams {
            yes_input_mode: true,
            no_input_mode: true,
            ..FilePermissionOptionsParams::default()
        });
        assert_eq!(options[0].value, "yes");
        assert_eq!(
            options[0].input_placeholder.as_deref(),
            Some("and tell nyan what to do next")
        );
        assert_eq!(options[1].value, "yes-session");
        assert_eq!(
            options[1].label,
            "Yes, allow all edits during this session (shift+tab)"
        );
        assert_eq!(options[2].value, "no");
        assert_eq!(
            options[2].input_placeholder.as_deref(),
            Some("and tell nyan what to do differently")
        );
    }

    #[test]
    fn permission_options_match_official_claude_folder_and_external_read_branches() {
        let claude = get_file_permission_options_from_params(&FilePermissionOptionsParams {
            in_claude_folder: true,
            ..FilePermissionOptionsParams::default()
        });
        assert_eq!(claude[1].value, "yes-claude-folder");
        assert_eq!(
            claude[1].option,
            PermissionOption::AcceptSession {
                scope: Some(PermissionSessionScope::ClaudeFolder)
            }
        );

        let read = get_file_permission_options_from_params(&FilePermissionOptionsParams {
            operation_type: FileOperationType::Read,
            in_allowed_working_path: false,
            directory_name: "outside".to_string(),
            ..FilePermissionOptionsParams::default()
        });
        assert_eq!(
            read[1].label,
            "Yes, allow reading from outside/ during this session"
        );
    }
}
