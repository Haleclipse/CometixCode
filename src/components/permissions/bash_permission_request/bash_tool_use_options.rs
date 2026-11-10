//! Maps to: CC
//! `components/permissions/BashPermissionRequest/bashToolUseOptions.tsx`.
//!
//! This keeps the official Bash permission option construction separate from
//! rendering. Runtime-only pieces (`shouldShowAlwaysAllowOptions`, classifier
//! feature flags, editable input callbacks) are supplied as explicit inputs so
//! the UI remains a pure projection of the permission decision state.

use crate::components::custom_select::SelectOptionData;
use crate::components::permissions::shell_permission_helpers::generate_shell_suggestions_label;
use crate::tools::bash_tool::tool_name::BASH_TOOL_NAME;
use crate::types::permissions::{PermissionRuleValue, PermissionUpdate};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BashToolUseOptionValue {
    Yes,
    YesApplySuggestions,
    YesPrefixEdited,
    YesClassifierReviewed,
    No,
}

impl BashToolUseOptionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::YesApplySuggestions => "yes-apply-suggestions",
            Self::YesPrefixEdited => "yes-prefix-edited",
            Self::YesClassifierReviewed => "yes-classifier-reviewed",
            Self::No => "no",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "yes-apply-suggestions" => Self::YesApplySuggestions,
            "yes-prefix-edited" => Self::YesPrefixEdited,
            "yes-classifier-reviewed" => Self::YesClassifierReviewed,
            "no" => Self::No,
            _ => Self::Yes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BashPermissionOptionKind {
    Select,
    Input {
        placeholder: String,
        initial_value: Option<String>,
        show_label_with_value: bool,
        label_value_separator: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashPermissionOption {
    pub label: String,
    pub value: BashToolUseOptionValue,
    pub kind: BashPermissionOptionKind,
    pub disabled: bool,
}

impl BashPermissionOption {
    fn select(label: impl Into<String>, value: BashToolUseOptionValue) -> Self {
        Self {
            label: label.into(),
            value,
            kind: BashPermissionOptionKind::Select,
            disabled: false,
        }
    }

    fn input(
        label: impl Into<String>,
        value: BashToolUseOptionValue,
        placeholder: impl Into<String>,
        initial_value: Option<String>,
        show_label_with_value: bool,
        label_value_separator: Option<&str>,
    ) -> Self {
        Self {
            label: label.into(),
            value,
            kind: BashPermissionOptionKind::Input {
                placeholder: placeholder.into(),
                initial_value,
                show_label_with_value,
                label_value_separator: label_value_separator.map(str::to_string),
            },
            disabled: false,
        }
    }

    /// Maps to: CC options fed to `<Select>` (BashPermissionRequest.tsx:575)
    /// — Input-kind options carry the CC `type: 'input'` display config.
    /// Adapter for the host's migration onto the shared Select; the host
    /// currently still self-renders via ListItem + TextInput.
    pub fn to_select_option(&self) -> SelectOptionData {
        let input = match &self.kind {
            BashPermissionOptionKind::Select => None,
            BashPermissionOptionKind::Input {
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
            disabled: self.disabled,
            input,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BashToolUseOptionsInput {
    pub suggestions: Vec<PermissionUpdate>,
    pub decision_reason_is_classifier: bool,
    pub classifier_review_options_enabled: bool,
    pub classifier_description: Option<String>,
    pub initial_classifier_description_empty: bool,
    pub existing_allow_descriptions: Vec<String>,
    pub yes_input_mode: bool,
    pub no_input_mode: bool,
    pub editable_prefix: Option<String>,
    pub editable_prefix_input_enabled: bool,
    pub show_always_allow_options: bool,
    pub current_cwd: String,
}

fn description_already_exists(description: &str, existing_descriptions: &[String]) -> bool {
    let normalized = description.to_lowercase().trim_end().to_string();
    existing_descriptions
        .iter()
        .any(|existing| existing.to_lowercase().trim_end() == normalized)
}

fn suggestion_rules(suggestions: &[PermissionUpdate]) -> Vec<&PermissionRuleValue> {
    suggestions
        .iter()
        .flat_map(|suggestion| match suggestion {
            PermissionUpdate::AddRules { rules, .. } => rules.iter().collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

fn has_non_bash_suggestions(suggestions: &[PermissionUpdate]) -> bool {
    suggestions.iter().any(|suggestion| {
        matches!(suggestion, PermissionUpdate::AddDirectories { .. })
            || match suggestion {
                PermissionUpdate::AddRules { rules, .. } => {
                    rules.iter().any(|rule| rule.tool_name != BASH_TOOL_NAME)
                }
                _ => false,
            }
    })
}

/// Maps to: CC `descriptionAlreadyExists(...)`.
pub fn bash_description_already_exists(
    description: &str,
    existing_descriptions: &[String],
) -> bool {
    description_already_exists(description, existing_descriptions)
}

/// Maps to: CC `stripBashRedirections(...)`.
pub fn strip_bash_redirections(command: &str) -> String {
    // This mirrors the display-only behavior of the official helper for the
    // common redirection forms used in generated labels. The shell parser slice
    // can replace this with `extractOutputRedirections` parity later.
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut skip_next = false;
    for token in tokens {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(
            token,
            ">" | ">>" | "1>" | "1>>" | "2>" | "2>>" | "&>" | "&>>"
        ) {
            skip_next = true;
            continue;
        }
        if token.starts_with('>')
            || token.starts_with("1>")
            || token.starts_with("2>")
            || token.starts_with("&>")
        {
            continue;
        }
        output.push(token);
    }
    output.join(" ")
}

/// Maps to: CC `bashToolUseOptions(...)`.
pub fn bash_tool_use_options(input: BashToolUseOptionsInput) -> Vec<BashPermissionOption> {
    let mut options = Vec::new();

    if input.yes_input_mode {
        options.push(BashPermissionOption::input(
            "Yes",
            BashToolUseOptionValue::Yes,
            "and tell nyan what to do next",
            None,
            false,
            None,
        ));
    } else {
        options.push(BashPermissionOption::select(
            "Yes",
            BashToolUseOptionValue::Yes,
        ));
    }

    if input.show_always_allow_options {
        if input.editable_prefix.is_some()
            && input.editable_prefix_input_enabled
            && !has_non_bash_suggestions(&input.suggestions)
            && !input.suggestions.is_empty()
        {
            options.push(BashPermissionOption::input(
                "Yes, and don’t ask again for",
                BashToolUseOptionValue::YesPrefixEdited,
                "command prefix (e.g., npm run:*)",
                input.editable_prefix.clone(),
                true,
                Some(": "),
            ));
        } else if !input.suggestions.is_empty()
            && let Some(label) = generate_shell_suggestions_label(
                &input.suggestions,
                BASH_TOOL_NAME,
                &input.current_cwd,
                Some(strip_bash_redirections),
            )
        {
            options.push(BashPermissionOption::select(
                label,
                BashToolUseOptionValue::YesApplySuggestions,
            ));
        }

        let editable_prefix_shown = options
            .iter()
            .any(|option| option.value == BashToolUseOptionValue::YesPrefixEdited);
        if input.classifier_review_options_enabled
            && !editable_prefix_shown
            && !input.initial_classifier_description_empty
            && !description_already_exists(
                input.classifier_description.as_deref().unwrap_or_default(),
                &input.existing_allow_descriptions,
            )
            && !input.decision_reason_is_classifier
        {
            options.push(BashPermissionOption::input(
                "Yes, and don’t ask again for",
                BashToolUseOptionValue::YesClassifierReviewed,
                "describe what to allow...",
                input.classifier_description.clone(),
                true,
                Some(": "),
            ));
        }
    }

    if input.no_input_mode {
        options.push(BashPermissionOption::input(
            "No",
            BashToolUseOptionValue::No,
            "and tell nyan what to do differently",
            None,
            false,
            None,
        ));
    } else {
        options.push(BashPermissionOption::select(
            "No",
            BashToolUseOptionValue::No,
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

    fn base_input() -> BashToolUseOptionsInput {
        BashToolUseOptionsInput {
            show_always_allow_options: true,
            current_cwd: "/repo".to_string(),
            ..BashToolUseOptionsInput::default()
        }
    }

    #[test]
    fn bash_tool_use_options_default_yes_no_match_official_order() {
        let options = bash_tool_use_options(base_input());
        assert_eq!(
            options
                .iter()
                .map(|option| option.value)
                .collect::<Vec<_>>(),
            vec![BashToolUseOptionValue::Yes, BashToolUseOptionValue::No]
        );
        assert_eq!(options[0].label, "Yes");
        assert_eq!(options[1].label, "No");
    }

    #[test]
    fn bash_tool_use_options_adds_editable_prefix_before_no() {
        let options = bash_tool_use_options(BashToolUseOptionsInput {
            suggestions: vec![add_rules(vec![PermissionRuleValue::new(
                BASH_TOOL_NAME,
                Some("npm run:*".to_string()),
            )])],
            editable_prefix: Some("npm run:*".to_string()),
            editable_prefix_input_enabled: true,
            ..base_input()
        });

        assert_eq!(options[1].value, BashToolUseOptionValue::YesPrefixEdited);
        assert_eq!(options[1].label, "Yes, and don’t ask again for");
        assert_eq!(
            options[1].kind,
            BashPermissionOptionKind::Input {
                placeholder: "command prefix (e.g., npm run:*)".to_string(),
                initial_value: Some("npm run:*".to_string()),
                show_label_with_value: true,
                label_value_separator: Some(": ".to_string()),
            }
        );
        assert_eq!(options[2].value, BashToolUseOptionValue::No);
    }

    #[test]
    fn bash_tool_use_options_uses_suggestions_label_when_prefix_input_not_available() {
        let options = bash_tool_use_options(BashToolUseOptionsInput {
            suggestions: vec![add_rules(vec![PermissionRuleValue::new(
                BASH_TOOL_NAME,
                Some("cargo:*".to_string()),
            )])],
            ..base_input()
        });

        assert_eq!(
            options[1].value,
            BashToolUseOptionValue::YesApplySuggestions
        );
        assert_eq!(
            options[1].label,
            "Yes, and don't ask again for cargo commands in /repo"
        );
    }

    #[test]
    fn bash_tool_use_options_uses_directory_label_instead_of_editable_prefix() {
        let options = bash_tool_use_options(BashToolUseOptionsInput {
            suggestions: vec![add_directories(vec!["/repo/logs"])],
            editable_prefix: Some("cargo:*".to_string()),
            editable_prefix_input_enabled: true,
            ..base_input()
        });

        assert_eq!(
            options[1].value,
            BashToolUseOptionValue::YesApplySuggestions
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
    fn bash_tool_use_options_suppresses_classifier_duplicate_descriptions() {
        let options = bash_tool_use_options(BashToolUseOptionsInput {
            classifier_review_options_enabled: true,
            classifier_description: Some("Run tests".to_string()),
            existing_allow_descriptions: vec!["run tests ".to_string()],
            ..base_input()
        });
        assert!(
            !options
                .iter()
                .any(|option| option.value == BashToolUseOptionValue::YesClassifierReviewed)
        );

        let options = bash_tool_use_options(BashToolUseOptionsInput {
            classifier_review_options_enabled: true,
            classifier_description: Some("Run tests".to_string()),
            ..base_input()
        });
        assert!(
            options
                .iter()
                .any(|option| option.value == BashToolUseOptionValue::YesClassifierReviewed)
        );
    }

    #[test]
    fn bash_tool_use_options_turns_feedback_modes_into_input_options() {
        let options = bash_tool_use_options(BashToolUseOptionsInput {
            yes_input_mode: true,
            no_input_mode: true,
            ..base_input()
        });

        assert_eq!(
            options[0].kind,
            BashPermissionOptionKind::Input {
                placeholder: "and tell nyan what to do next".to_string(),
                initial_value: None,
                show_label_with_value: false,
                label_value_separator: None,
            }
        );
        assert_eq!(
            options.last().expect("no option").kind,
            BashPermissionOptionKind::Input {
                placeholder: "and tell nyan what to do differently".to_string(),
                initial_value: None,
                show_label_with_value: false,
                label_value_separator: None,
            }
        );
    }
}
