//! Maps to: CC `components/TrustDialog/utils.ts`.
//!
//! Read-only source-classification helpers used by the workspace trust dialog.
//! They identify project/local settings that can execute code or redirect
//! traffic; they do not mutate trust state or execute configured commands.

use crate::tools::bash_tool::tool_name::BASH_TOOL_NAME;
use crate::utils::managed_env::is_safe_managed_env_var;
use crate::utils::settings::{SettingsJson, types::PermissionsSettings};

const PROJECT_SETTINGS_LABEL: &str = ".claude/settings.json";
const LOCAL_SETTINGS_LABEL: &str = ".claude/settings.local.json";

fn has_hooks(settings: Option<&SettingsJson>) -> bool {
    let Some(settings) = settings else {
        return false;
    };
    if settings.disable_all_hooks.unwrap_or(false) {
        return false;
    }
    if settings.status_line.is_some() || settings.file_suggestion.is_some() {
        return true;
    }
    let Some(hooks) = settings.hooks.as_ref() else {
        return false;
    };
    hooks.as_object().is_some_and(|object| {
        object.values().any(|hook_config| {
            hook_config
                .as_array()
                .is_some_and(|array| !array.is_empty())
        })
    })
}

fn source_pair<'a>(
    project_settings: Option<&'a SettingsJson>,
    local_settings: Option<&'a SettingsJson>,
) -> [(Option<&'a SettingsJson>, &'static str); 2] {
    [
        (project_settings, PROJECT_SETTINGS_LABEL),
        (local_settings, LOCAL_SETTINGS_LABEL),
    ]
}

/// Maps to: CC `utils.ts#getHooksSources()`.
pub fn get_hooks_sources_from_settings(
    project_settings: Option<&SettingsJson>,
    local_settings: Option<&SettingsJson>,
) -> Vec<String> {
    source_pair(project_settings, local_settings)
        .into_iter()
        .filter(|&(settings, _)| has_hooks(settings))
        .map(|(_, label)| label.to_string())
        .collect()
}

fn has_bash_permission_settings(settings: Option<&PermissionsSettings>) -> bool {
    settings
        .and_then(|permissions| permissions.allow.as_ref())
        .is_some_and(|rules| {
            rules.iter().any(|rule| {
                rule == BASH_TOOL_NAME || rule.starts_with(&format!("{BASH_TOOL_NAME}("))
            })
        })
}

/// Maps to: CC `utils.ts#getBashPermissionSources()`.
pub fn get_bash_permission_sources_from_settings(
    project_settings: Option<&SettingsJson>,
    local_settings: Option<&SettingsJson>,
) -> Vec<String> {
    source_pair(project_settings, local_settings)
        .into_iter()
        .filter(|&(settings, _)| {
            has_bash_permission_settings(
                settings.and_then(|settings| settings.permissions.as_ref()),
            )
        })
        .map(|(_, label)| label.to_string())
        .collect()
}

fn has_otel_headers_helper(settings: Option<&SettingsJson>) -> bool {
    settings
        .and_then(|settings| settings.otel_headers_helper.as_deref())
        .is_some_and(|value| !value.is_empty())
}

/// Maps to: CC `utils.ts#getOtelHeadersHelperSources()`.
pub fn get_otel_headers_helper_sources_from_settings(
    project_settings: Option<&SettingsJson>,
    local_settings: Option<&SettingsJson>,
) -> Vec<String> {
    source_pair(project_settings, local_settings)
        .into_iter()
        .filter(|&(settings, _)| has_otel_headers_helper(settings))
        .map(|(_, label)| label.to_string())
        .collect()
}

fn has_api_key_helper(settings: Option<&SettingsJson>) -> bool {
    settings
        .and_then(|settings| settings.api_key_helper.as_deref())
        .is_some_and(|value| !value.is_empty())
}

/// Maps to: CC `utils.ts#getApiKeyHelperSources()`.
pub fn get_api_key_helper_sources_from_settings(
    project_settings: Option<&SettingsJson>,
    local_settings: Option<&SettingsJson>,
) -> Vec<String> {
    source_pair(project_settings, local_settings)
        .into_iter()
        .filter(|&(settings, _)| has_api_key_helper(settings))
        .map(|(_, label)| label.to_string())
        .collect()
}

fn has_aws_commands(settings: Option<&SettingsJson>) -> bool {
    settings.is_some_and(|settings| {
        settings
            .aws_auth_refresh
            .as_deref()
            .is_some_and(|value| !value.is_empty())
            || settings
                .aws_credential_export
                .as_deref()
                .is_some_and(|value| !value.is_empty())
    })
}

/// Maps to: CC `utils.ts#getAwsCommandsSources()`.
pub fn get_aws_commands_sources_from_settings(
    project_settings: Option<&SettingsJson>,
    local_settings: Option<&SettingsJson>,
) -> Vec<String> {
    source_pair(project_settings, local_settings)
        .into_iter()
        .filter(|&(settings, _)| has_aws_commands(settings))
        .map(|(_, label)| label.to_string())
        .collect()
}

fn has_gcp_commands(settings: Option<&SettingsJson>) -> bool {
    settings
        .and_then(|settings| settings.gcp_auth_refresh.as_deref())
        .is_some_and(|value| !value.is_empty())
}

/// Maps to: CC `utils.ts#getGcpCommandsSources()`.
pub fn get_gcp_commands_sources_from_settings(
    project_settings: Option<&SettingsJson>,
    local_settings: Option<&SettingsJson>,
) -> Vec<String> {
    source_pair(project_settings, local_settings)
        .into_iter()
        .filter(|&(settings, _)| has_gcp_commands(settings))
        .map(|(_, label)| label.to_string())
        .collect()
}

fn has_dangerous_env_vars(settings: Option<&SettingsJson>) -> bool {
    settings
        .and_then(|settings| settings.env.as_ref())
        .is_some_and(|env| env.keys().any(|key| !is_safe_managed_env_var(key)))
}

/// Maps to: CC `utils.ts#getDangerousEnvVarsSources()`.
pub fn get_dangerous_env_vars_sources_from_settings(
    project_settings: Option<&SettingsJson>,
    local_settings: Option<&SettingsJson>,
) -> Vec<String> {
    source_pair(project_settings, local_settings)
        .into_iter()
        .filter(|&(settings, _)| has_dangerous_env_vars(settings))
        .map(|(_, label)| label.to_string())
        .collect()
}

/// Maps to: CC `utils.ts#formatListWithAnd(...)`.
pub fn format_list_with_and(items: &[String], limit: Option<usize>) -> String {
    if items.is_empty() {
        return String::new();
    }
    let effective_limit = limit.filter(|limit| *limit != 0);
    if effective_limit.is_none_or(|limit| items.len() <= limit) {
        return match items.len() {
            1 => items[0].clone(),
            2 => format!("{} and {}", items[0], items[1]),
            _ => {
                let last = items.last().expect("non-empty");
                let all_but_last = &items[..items.len() - 1];
                format!("{}, and {last}", all_but_last.join(", "))
            }
        };
    }

    let limit = effective_limit.expect("checked above");
    let shown = &items[..limit.min(items.len())];
    let remaining = items.len().saturating_sub(limit);
    if shown.len() == 1 {
        format!("{} and {remaining} more", shown[0])
    } else {
        format!("{}, and {remaining} more", shown.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_sources_match_official_project_local_scopes() {
        let project = SettingsJson {
            hooks: Some(serde_json::json!({ "PreToolUse": [{ "command": "echo" }] })),
            permissions: Some(PermissionsSettings {
                allow: Some(vec!["Bash(git status)".to_string()]),
                ..PermissionsSettings::default()
            }),
            api_key_helper: Some("helper".to_string()),
            env: Some(std::sync::Arc::new(indexmap::IndexMap::from([(
                "ANTHROPIC_BASE_URL".to_string(),
                "https://proxy".to_string(),
            )]))),
            ..SettingsJson::default()
        };
        let local = SettingsJson {
            aws_credential_export: Some("aws export".to_string()),
            gcp_auth_refresh: Some("gcloud auth".to_string()),
            otel_headers_helper: Some("otel".to_string()),
            ..SettingsJson::default()
        };

        assert_eq!(
            get_hooks_sources_from_settings(Some(&project), Some(&local)),
            vec![PROJECT_SETTINGS_LABEL]
        );
        assert_eq!(
            get_bash_permission_sources_from_settings(Some(&project), Some(&local)),
            vec![PROJECT_SETTINGS_LABEL]
        );
        assert_eq!(
            get_api_key_helper_sources_from_settings(Some(&project), Some(&local)),
            vec![PROJECT_SETTINGS_LABEL]
        );
        assert_eq!(
            get_aws_commands_sources_from_settings(Some(&project), Some(&local)),
            vec![LOCAL_SETTINGS_LABEL]
        );
        assert_eq!(
            get_gcp_commands_sources_from_settings(Some(&project), Some(&local)),
            vec![LOCAL_SETTINGS_LABEL]
        );
        assert_eq!(
            get_otel_headers_helper_sources_from_settings(Some(&project), Some(&local)),
            vec![LOCAL_SETTINGS_LABEL]
        );
        assert_eq!(
            get_dangerous_env_vars_sources_from_settings(Some(&project), Some(&local)),
            vec![PROJECT_SETTINGS_LABEL]
        );
    }

    #[test]
    fn format_list_with_and_matches_official_conjunction_and_limit_rules() {
        assert_eq!(format_list_with_and(&[], None), "");
        assert_eq!(format_list_with_and(&["a".to_string()], None), "a");
        assert_eq!(
            format_list_with_and(&["a".to_string(), "b".to_string()], None),
            "a and b"
        );
        assert_eq!(
            format_list_with_and(&["a".to_string(), "b".to_string(), "c".to_string()], None),
            "a, b, and c"
        );
        assert_eq!(
            format_list_with_and(
                &["a".to_string(), "b".to_string(), "c".to_string()],
                Some(1)
            ),
            "a and 2 more"
        );
        assert_eq!(
            format_list_with_and(
                &["a".to_string(), "b".to_string(), "c".to_string()],
                Some(0)
            ),
            "a, b, and c"
        );
    }
}
