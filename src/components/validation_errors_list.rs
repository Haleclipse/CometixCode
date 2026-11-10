//! Maps to: CC `components/ValidationErrorsList.tsx`.
//!
//! The official component groups validation errors by file, sorts paths,
//! treeifies dot-notation paths, and deduplicates suggestion/docLink pairs. This
//! port preserves that data shaping and renders it with iocraft text colors.
//! @nyan: errors carry no docLink (see `ValidationTip`), so only suggestions
//! are deduplicated and there is no "Learn more" row.

use crate::utils::settings::ValidationError;
use crate::utils::theme::Theme;
use crate::utils::treeify::{TreeNode, TreeifyOptions, set_dot_path, treeify};
use iocraft::prelude::*;
use std::collections::{BTreeMap, HashSet};

/// Maps to: CC `components/ValidationErrorsList.tsx` `buildNestedTree`.
pub fn build_nested_tree(errors: &[ValidationError]) -> TreeNode {
    let mut tree = TreeNode::new();

    for error in errors {
        if error.path.is_empty() {
            set_dot_path(&mut tree, "", error.message.clone());
            continue;
        }

        let mut modified_path = error.path.clone();
        if let Some(invalid_value) = &error.invalid_value {
            let path_parts: Vec<&str> = error.path.split('.').collect();
            if !path_parts.is_empty() {
                let mut new_path_parts = Vec::new();
                for (index, part) in path_parts.iter().enumerate() {
                    if part.is_empty() {
                        continue;
                    }
                    let is_numeric = part.parse::<usize>().is_ok();
                    if is_numeric && index == path_parts.len().saturating_sub(1) {
                        new_path_parts.push(format!("\"{invalid_value}\""));
                    } else {
                        new_path_parts.push((*part).to_string());
                    }
                }
                modified_path = new_path_parts.join(".");
            }
        }

        set_dot_path(&mut tree, &modified_path, error.message.clone());
    }

    tree
}

/// Maps to: CC `components/ValidationErrorsList.tsx` suggestion/docLink pair
/// deduplication.
pub fn unique_suggestions(errors: &[ValidationError]) -> Vec<String> {
    let mut seen = HashSet::new();
    errors
        .iter()
        .filter_map(|error| error.suggestion.clone())
        .filter(|suggestion| seen.insert(suggestion.clone()))
        .collect()
}

fn sorted_errors_by_file(errors: &[ValidationError]) -> BTreeMap<String, Vec<ValidationError>> {
    let mut errors_by_file: BTreeMap<String, Vec<ValidationError>> = BTreeMap::new();
    for error in errors {
        let file = error
            .file
            .clone()
            .unwrap_or_else(|| "(file not specified)".to_string());
        errors_by_file.entry(file).or_default().push(error.clone());
    }

    for file_errors in errors_by_file.values_mut() {
        file_errors.sort_by(|a, b| match (a.path.is_empty(), b.path.is_empty()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.path.cmp(&b.path),
        });
    }

    errors_by_file
}

#[derive(Default, Props)]
pub struct ValidationErrorsListProps {
    pub errors: Vec<ValidationError>,
}

/// Maps to: CC `components/ValidationErrorsList.tsx` `ValidationErrorsList`.
#[component]
pub fn ValidationErrorsList(
    props: &mut ValidationErrorsListProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    if props.errors.is_empty() {
        return element! { View() }.into_any();
    }

    let errors_by_file = sorted_errors_by_file(&props.errors);

    element! {
        View(flex_direction: FlexDirection::Column) {
            #(errors_by_file.into_iter().map(|(file, file_errors)| {
                let error_tree = build_nested_tree(&file_errors);
                let tree_output = treeify(&error_tree, TreeifyOptions::default());
                let suggestions = unique_suggestions(&file_errors);
                element! {
                    View(flex_direction: FlexDirection::Column) {
                        Text(content: file)
                        View(padding_left: 1u32) {
                            Text(content: tree_output, color: theme.inactive, wrap: TextWrap::Wrap)
                        }
                        #(if suggestions.is_empty() {
                            None
                        } else {
                            Some(element! {
                                View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                                    #(suggestions.into_iter().map(|suggestion| element! {
                                        View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                                            Text(content: suggestion, color: theme.inactive, wrap: TextWrap::Wrap)
                                        }
                                    }))
                                }
                            })
                        })
                    }
                }
            }))
        }
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn validation_error(
        file: Option<&str>,
        path: &str,
        message: &str,
        invalid_value: Option<&str>,
        suggestion: Option<&str>,
    ) -> ValidationError {
        ValidationError {
            file: file.map(|s| s.to_string()),
            path: path.to_string(),
            message: message.to_string(),
            expected: None,
            invalid_value: invalid_value.map(|s| s.to_string()),
            suggestion: suggestion.map(|s| s.to_string()),
        }
    }

    #[test]
    fn validation_errors_build_nested_tree_enhances_numeric_invalid_value_paths() {
        let errors = vec![validation_error(
            Some("settings.json"),
            "permissions.allow.0",
            "Invalid permission rule",
            Some("BadRule"),
            None,
        )];

        let tree = build_nested_tree(&errors);
        let text = treeify(&tree, TreeifyOptions::default());

        assert!(text.contains("permissions"), "tree=\n{text}");
        assert!(text.contains("allow"), "tree=\n{text}");
        assert!(
            text.contains("\"BadRule\": Invalid permission rule"),
            "tree=\n{text}"
        );
    }

    #[test]
    fn validation_errors_list_deduplicates_suggestions() {
        let errors = vec![
            validation_error(
                Some("a.json"),
                "model",
                "Invalid model",
                None,
                Some("Use a supported model"),
            ),
            validation_error(
                Some("a.json"),
                "theme",
                "Invalid theme",
                None,
                Some("Use a supported model"),
            ),
            validation_error(Some("a.json"), "env", "Invalid env", None, None),
        ];

        assert_eq!(unique_suggestions(&errors), ["Use a supported model"]);
    }

    #[test]
    fn validation_errors_list_renders_grouped_files_tree_and_suggestions() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ValidationErrorsList(errors: vec![
                    validation_error(
                        Some("b.json"),
                        "theme",
                        "Invalid theme",
                        None,
                        None,
                    ),
                    validation_error(
                        Some("a.json"),
                        "",
                        "Invalid or malformed JSON",
                        None,
                        Some("Fix the JSON syntax"),
                    ),
                ])
            }
        }
        .render(Some(100))
        .to_string();

        assert!(text.contains("a.json"), "canvas=\n{text}");
        assert!(text.contains("b.json"), "canvas=\n{text}");
        assert!(
            text.find("a.json").unwrap() < text.find("b.json").unwrap(),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("└ Invalid or malformed JSON"),
            "canvas=\n{text}"
        );
        assert!(text.contains("└ theme: Invalid theme"), "canvas=\n{text}");
        assert!(text.contains("Fix the JSON syntax"), "canvas=\n{text}");
        assert!(!text.contains("Learn more"), "canvas=\n{text}");
    }
}
