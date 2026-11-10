//! Maps to: CC
//! `components/permissions/PowerShellPermissionRequest/powershellToolUseOptions.tsx`.
//!
//! Keeps PowerShell permission option construction separate from rendering.
//! Runtime hooks such as `shouldShowAlwaysAllowOptions()` and editable input
//! callbacks are supplied as explicit props by the caller.

use crate::components::custom_select::SelectOptionData;
use crate::components::permissions::shell_permission_helpers::generate_shell_suggestions_label;
use crate::types::permissions::PermissionUpdate;

pub const POWERSHELL_TOOL_NAME: &str = "PowerShell";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerShellToolUseOptionValue {
    Yes,
    YesApplySuggestions,
    YesPrefixEdited,
    No,
}

impl PowerShellToolUseOptionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::YesApplySuggestions => "yes-apply-suggestions",
            Self::YesPrefixEdited => "yes-prefix-edited",
            Self::No => "no",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "yes-apply-suggestions" => Self::YesApplySuggestions,
            "yes-prefix-edited" => Self::YesPrefixEdited,
            "no" => Self::No,
            _ => Self::Yes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerShellPermissionOptionKind {
    Select,
    Input {
        placeholder: String,
        initial_value: Option<String>,
        show_label_with_value: bool,
        label_value_separator: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerShellPermissionOption {
    pub label: String,
    pub value: PowerShellToolUseOptionValue,
    pub kind: PowerShellPermissionOptionKind,
}

impl PowerShellPermissionOption {
    fn select(label: impl Into<String>, value: PowerShellToolUseOptionValue) -> Self {
        Self {
            label: label.into(),
            value,
            kind: PowerShellPermissionOptionKind::Select,
        }
    }

    fn input(
        label: impl Into<String>,
        value: PowerShellToolUseOptionValue,
        placeholder: impl Into<String>,
        initial_value: Option<String>,
        show_label_with_value: bool,
        label_value_separator: Option<&str>,
    ) -> Self {
        Self {
            label: label.into(),
            value,
            kind: PowerShellPermissionOptionKind::Input {
                placeholder: placeholder.into(),
                initial_value,
                show_label_with_value,
                label_value_separator: label_value_separator.map(str::to_string),
            },
        }
    }

    /// Maps to: CC options fed to `<Select>`
    /// (PowerShellPermissionRequest.tsx:286) — Input-kind options carry the
    /// CC `type: 'input'` display config. Adapter for the host's migration
    /// onto the shared Select; the host currently still self-renders via
    /// ListItem + TextInput.
    pub fn to_select_option(&self) -> SelectOptionData {
        let input = match &self.kind {
            PowerShellPermissionOptionKind::Select => None,
            PowerShellPermissionOptionKind::Input {
                placeholder,
                initial_value,
                show_label_with_value,
                label_value_separator,
            } => Some(crate::components::custom_select::SelectInputOptionData {
                placeholder: Some(placeholder.clone()),
                value: initial_value.clone().unwrap_or_default(),
                show_label_with_value: *show_label_with_value,
                label_value_separator: label_value_separator.clone(),
            }),
        };
        SelectOptionData {
            label: self.label.clone(),
            description: None,
            dim_description: true,
            value: self.value.as_str().to_string(),
            disabled: false,
            input,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PowerShellToolUseOptionsInput {
    pub suggestions: Vec<PermissionUpdate>,
    pub yes_input_mode: bool,
    pub no_input_mode: bool,
    pub editable_prefix: Option<String>,
    pub editable_prefix_input_enabled: bool,
    pub show_always_allow_options: bool,
    pub current_cwd: String,
}

fn has_non_powershell_suggestions(suggestions: &[PermissionUpdate]) -> bool {
    suggestions.iter().any(|suggestion| {
        matches!(suggestion, PermissionUpdate::AddDirectories { .. })
            || match suggestion {
                PermissionUpdate::AddRules { rules, .. } => rules
                    .iter()
                    .any(|rule| rule.tool_name != POWERSHELL_TOOL_NAME),
                _ => false,
            }
    })
}

/// Maps to: CC `powershellToolUseOptions(...)`.
pub fn powershell_tool_use_options(
    input: PowerShellToolUseOptionsInput,
) -> Vec<PowerShellPermissionOption> {
    let mut options = Vec::new();

    if input.yes_input_mode {
        options.push(PowerShellPermissionOption::input(
            "Yes",
            PowerShellToolUseOptionValue::Yes,
            "and tell nyan what to do next",
            None,
            false,
            None,
        ));
    } else {
        options.push(PowerShellPermissionOption::select(
            "Yes",
            PowerShellToolUseOptionValue::Yes,
        ));
    }

    if input.show_always_allow_options && !input.suggestions.is_empty() {
        if input.editable_prefix.is_some()
            && input.editable_prefix_input_enabled
            && !has_non_powershell_suggestions(&input.suggestions)
        {
            options.push(PowerShellPermissionOption::input(
                "Yes, and don’t ask again for",
                PowerShellToolUseOptionValue::YesPrefixEdited,
                "command prefix (e.g., Get-Process:*)",
                input.editable_prefix.clone(),
                true,
                Some(": "),
            ));
        } else if let Some(label) = generate_shell_suggestions_label(
            &input.suggestions,
            POWERSHELL_TOOL_NAME,
            &input.current_cwd,
            None,
        ) {
            options.push(PowerShellPermissionOption::select(
                label,
                PowerShellToolUseOptionValue::YesApplySuggestions,
            ));
        }
    }

    if input.no_input_mode {
        options.push(PowerShellPermissionOption::input(
            "No",
            PowerShellToolUseOptionValue::No,
            "and tell nyan what to do differently",
            None,
            false,
            None,
        ));
    } else {
        options.push(PowerShellPermissionOption::select(
            "No",
            PowerShellToolUseOptionValue::No,
        ));
    }

    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionBehavior, PermissionRuleValue, PermissionUpdateDestination,
    };

    fn add_rules(rules: Vec<PermissionRuleValue>) -> PermissionUpdate {
        PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::LocalSettings,
            behavior: PermissionBehavior::Allow,
            rules,
        }
    }

    fn add_directories(directories: Vec<&str>) -> PermissionUpdate {
        PermissionUpdate::AddDirectories {
            destination: PermissionUpdateDestination::LocalSettings,
            directories: directories.into_iter().map(str::to_string).collect(),
        }
    }

    fn base_input() -> PowerShellToolUseOptionsInput {
        PowerShellToolUseOptionsInput {
            show_always_allow_options: true,
            current_cwd: "C:/repo".to_string(),
            ..PowerShellToolUseOptionsInput::default()
        }
    }

    #[test]
    fn powershell_tool_use_options_default_yes_no_match_official_order() {
        let options = powershell_tool_use_options(base_input());
        assert_eq!(
            options
                .iter()
                .map(|option| option.value)
                .collect::<Vec<_>>(),
            vec![
                PowerShellToolUseOptionValue::Yes,
                PowerShellToolUseOptionValue::No
            ]
        );
        assert_eq!(options[0].label, "Yes");
        assert_eq!(options[1].label, "No");
    }

    #[test]
    fn powershell_tool_use_options_adds_editable_prefix_before_no() {
        let options = powershell_tool_use_options(PowerShellToolUseOptionsInput {
            suggestions: vec![add_rules(vec![PermissionRuleValue::new(
                POWERSHELL_TOOL_NAME,
                Some("Get-Process:*".to_string()),
            )])],
            editable_prefix: Some("Get-Process:*".to_string()),
            editable_prefix_input_enabled: true,
            ..base_input()
        });

        assert_eq!(
            options[1].value,
            PowerShellToolUseOptionValue::YesPrefixEdited
        );
        assert_eq!(options[1].label, "Yes, and don’t ask again for");
        assert_eq!(
            options[1].kind,
            PowerShellPermissionOptionKind::Input {
                placeholder: "command prefix (e.g., Get-Process:*)".to_string(),
                initial_value: Some("Get-Process:*".to_string()),
                show_label_with_value: true,
                label_value_separator: Some(": ".to_string()),
            }
        );
        assert_eq!(options[2].value, PowerShellToolUseOptionValue::No);
    }

    #[test]
    fn powershell_tool_use_options_uses_suggestions_label_when_prefix_unavailable() {
        let options = powershell_tool_use_options(PowerShellToolUseOptionsInput {
            suggestions: vec![add_rules(vec![PermissionRuleValue::new(
                POWERSHELL_TOOL_NAME,
                Some("Get-ChildItem:*".to_string()),
            )])],
            ..base_input()
        });

        assert_eq!(
            options[1].value,
            PowerShellToolUseOptionValue::YesApplySuggestions
        );
        assert_eq!(
            options[1].label,
            "Yes, and don't ask again for Get-ChildItem commands in C:/repo"
        );
    }

    #[test]
    fn powershell_tool_use_options_falls_back_to_directory_label_for_non_shell_suggestions() {
        let options = powershell_tool_use_options(PowerShellToolUseOptionsInput {
            suggestions: vec![add_directories(vec!["C:/repo/logs"])],
            editable_prefix: Some("Get-ChildItem:*".to_string()),
            editable_prefix_input_enabled: true,
            ..base_input()
        });

        assert_eq!(
            options[1].value,
            PowerShellToolUseOptionValue::YesApplySuggestions
        );
        assert_eq!(
            options[1].label,
            format!(
                "Yes, and always allow access to logs{} from this project",
                std::path::MAIN_SEPARATOR
            )
        );
    }

    #[test]
    fn powershell_tool_use_options_turns_feedback_modes_into_input_options() {
        let options = powershell_tool_use_options(PowerShellToolUseOptionsInput {
            yes_input_mode: true,
            no_input_mode: true,
            ..base_input()
        });

        assert_eq!(
            options[0].kind,
            PowerShellPermissionOptionKind::Input {
                placeholder: "and tell nyan what to do next".to_string(),
                initial_value: None,
                show_label_with_value: false,
                label_value_separator: None,
            }
        );
        assert_eq!(
            options.last().expect("no option").kind,
            PowerShellPermissionOptionKind::Input {
                placeholder: "and tell nyan what to do differently".to_string(),
                initial_value: None,
                show_label_with_value: false,
                label_value_separator: None,
            }
        );
    }
}
