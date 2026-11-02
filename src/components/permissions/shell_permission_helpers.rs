//! Maps to: CC `components/permissions/shellPermissionHelpers.tsx`.
//!
//! Official shell permission dialogs share this label builder for Bash and
//! PowerShell. The Rust port keeps the helper pure: callers provide the
//! already-computed permission suggestions and the current working directory.
//! Directory permission suggestions (`addDirectories`) are parsed and rendered;
//! applying them remains the permission response owner's responsibility.

use crate::types::permissions::PermissionUpdate;
use crate::utils::permissions::shell_rule_matching::permission_rule_extract_prefix;
use std::collections::BTreeSet;

fn basename_or_path(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// Maps to: CC `commandListDisplay(...)` plus
/// `commandListDisplayTruncated(...)`.
pub fn command_list_display_truncated(commands: &[String]) -> String {
    if commands.join(", ").chars().count() > 50 {
        return "similar".to_string();
    }

    match commands.len() {
        0 => String::new(),
        1 => commands[0].clone(),
        2 => format!("{} and {}", commands[0], commands[1]),
        _ => {
            let head = commands[..commands.len() - 1].join(", ");
            format!("{head}, and {}", commands[commands.len() - 1])
        }
    }
}

/// Maps to: CC `formatPathList(...)`.
pub fn format_path_list(paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }

    let sep = std::path::MAIN_SEPARATOR;
    let names = paths
        .iter()
        .map(|path| basename_or_path(path))
        .collect::<Vec<_>>();
    match names.len() {
        1 => format!("{}{sep}", names[0]),
        2 => format!("{}{sep} and {}{sep}", names[0], names[1]),
        _ => format!(
            "{}{sep}, {}{sep} and {} more",
            names[0],
            names[1],
            names.len().saturating_sub(2)
        ),
    }
}

fn add_rule_entries(
    suggestions: &[PermissionUpdate],
) -> Vec<&crate::types::permissions::PermissionRuleValue> {
    suggestions
        .iter()
        .flat_map(|suggestion| match suggestion {
            PermissionUpdate::AddRules { rules, .. } => rules.iter().collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

fn add_directory_entries(suggestions: &[PermissionUpdate]) -> Vec<String> {
    suggestions
        .iter()
        .flat_map(|suggestion| match suggestion {
            PermissionUpdate::AddDirectories { directories, .. } => directories.clone(),
            _ => Vec::new(),
        })
        .collect()
}

/// Maps to: CC `generateShellSuggestionsLabel(...)`.
pub fn generate_shell_suggestions_label(
    suggestions: &[PermissionUpdate],
    shell_tool_name: &str,
    current_cwd: &str,
    command_transform: Option<fn(&str) -> String>,
) -> Option<String> {
    let all_rules = add_rule_entries(suggestions);
    let read_rules = all_rules
        .iter()
        .copied()
        .filter(|rule| rule.tool_name == "Read")
        .collect::<Vec<_>>();
    let shell_rules = all_rules
        .iter()
        .copied()
        .filter(|rule| rule.tool_name == shell_tool_name)
        .collect::<Vec<_>>();

    let directories = add_directory_entries(suggestions);

    let read_paths = read_rules
        .into_iter()
        .filter_map(|rule| rule.rule_content.as_deref())
        .map(|content| content.strip_suffix("/**").unwrap_or(content).to_string())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();

    let mut seen = BTreeSet::new();
    let shell_commands = shell_rules
        .into_iter()
        .filter_map(|rule| rule.rule_content.as_deref())
        .map(|rule_content| permission_rule_extract_prefix(rule_content).unwrap_or(rule_content))
        .map(|command| {
            command_transform
                .map(|transform| transform(command))
                .unwrap_or_else(|| command.to_string())
        })
        .filter(|command| seen.insert(command.clone()))
        .collect::<Vec<_>>();

    let has_directories = !directories.is_empty();
    let has_read_paths = !read_paths.is_empty();
    let has_commands = !shell_commands.is_empty();

    if has_read_paths && !has_directories && !has_commands {
        if read_paths.len() == 1 {
            let dir_name = basename_or_path(&read_paths[0]);
            return Some(format!(
                "Yes, allow reading from {}{} from this project",
                dir_name,
                std::path::MAIN_SEPARATOR
            ));
        }
        return Some(format!(
            "Yes, allow reading from {} from this project",
            format_path_list(&read_paths)
        ));
    }

    if has_directories && !has_read_paths && !has_commands {
        if directories.len() == 1 {
            let dir_name = basename_or_path(&directories[0]);
            return Some(format!(
                "Yes, and always allow access to {}{} from this project",
                dir_name,
                std::path::MAIN_SEPARATOR
            ));
        }
        return Some(format!(
            "Yes, and always allow access to {} from this project",
            format_path_list(&directories)
        ));
    }

    if has_commands && !has_directories && !has_read_paths {
        return Some(format!(
            "Yes, and don't ask again for {} commands in {current_cwd}",
            command_list_display_truncated(&shell_commands)
        ));
    }

    if (has_directories || has_read_paths) && !has_commands {
        let all_paths = directories
            .iter()
            .chain(read_paths.iter())
            .cloned()
            .collect::<Vec<_>>();
        if has_directories && has_read_paths {
            return Some(format!(
                "Yes, and always allow access to {} from this project",
                format_path_list(&all_paths)
            ));
        }
    }

    if (has_directories || has_read_paths) && has_commands {
        let all_paths = directories
            .iter()
            .chain(read_paths.iter())
            .cloned()
            .collect::<Vec<_>>();
        if all_paths.len() == 1 && shell_commands.len() == 1 {
            return Some(format!(
                "Yes, and allow access to {} and {} commands",
                format_path_list(&all_paths),
                command_list_display_truncated(&shell_commands)
            ));
        }
        return Some(format!(
            "Yes, and allow {} access and {} commands",
            format_path_list(&all_paths),
            command_list_display_truncated(&shell_commands)
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionBehavior, PermissionRuleValue, PermissionUpdate, PermissionUpdateDestination,
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

    #[test]
    fn command_list_display_matches_official_plain_text_joining() {
        assert_eq!(command_list_display_truncated(&[]), "");
        assert_eq!(command_list_display_truncated(&["git".to_string()]), "git");
        assert_eq!(
            command_list_display_truncated(&["git".to_string(), "cargo".to_string()]),
            "git and cargo"
        );
        assert_eq!(
            command_list_display_truncated(&[
                "git".to_string(),
                "cargo".to_string(),
                "npm".to_string(),
            ]),
            "git, cargo, and npm"
        );
        assert_eq!(
            command_list_display_truncated(&[
                "a-very-long-command-name-that-exceeds-the-ui-budget".to_string(),
                "another-long-command".to_string()
            ]),
            "similar"
        );
    }

    #[test]
    fn shell_suggestions_label_matches_official_shell_only_copy() {
        let label = generate_shell_suggestions_label(
            &[add_rules(vec![PermissionRuleValue::new(
                "Bash",
                Some("cargo:*".to_string()),
            )])],
            "Bash",
            "/repo",
            None,
        )
        .expect("label");

        assert_eq!(
            label,
            "Yes, and don't ask again for cargo commands in /repo"
        );
    }

    #[test]
    fn shell_suggestions_label_matches_official_read_directory_and_mixed_copy() {
        let read_only = generate_shell_suggestions_label(
            &[add_rules(vec![PermissionRuleValue::new(
                "Read",
                Some("/repo/src/**".to_string()),
            )])],
            "Bash",
            "/repo",
            None,
        )
        .expect("read label");
        assert_eq!(
            read_only,
            format!(
                "Yes, allow reading from src{} from this project",
                std::path::MAIN_SEPARATOR
            )
        );

        let mixed = generate_shell_suggestions_label(
            &[add_rules(vec![
                PermissionRuleValue::new("Read", Some("/repo/src/**".to_string())),
                PermissionRuleValue::new("Bash", Some("cargo:*".to_string())),
            ])],
            "Bash",
            "/repo",
            None,
        )
        .expect("mixed label");
        assert_eq!(
            mixed,
            format!(
                "Yes, and allow access to src{} and cargo commands",
                std::path::MAIN_SEPARATOR
            )
        );

        let directories_only = generate_shell_suggestions_label(
            &[add_directories(vec![
                "/repo/logs",
                "/repo/tmp",
                "/repo/cache",
            ])],
            "Bash",
            "/repo",
            None,
        )
        .expect("directory label");
        assert_eq!(
            directories_only,
            format!(
                "Yes, and always allow access to logs{}, tmp{} and 1 more from this project",
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR
            )
        );

        let directory_and_read = generate_shell_suggestions_label(
            &[
                add_directories(vec!["/repo/logs"]),
                add_rules(vec![PermissionRuleValue::new(
                    "Read",
                    Some("/repo/src/**".to_string()),
                )]),
            ],
            "Bash",
            "/repo",
            None,
        )
        .expect("directory plus read label");
        assert_eq!(
            directory_and_read,
            format!(
                "Yes, and always allow access to logs{} and src{} from this project",
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR
            )
        );
    }
}
