//! Maps to: CC `components/ManagedSettingsSecurityDialog/utils.ts`.
//!
//! Pure extraction helpers for the managed-settings approval dialog. The
//! official module only classifies dangerous managed settings and formats names;
//! it does not perform writes or process exits.

use crate::utils::managed_env::{DANGEROUS_SHELL_SETTINGS, is_safe_managed_env_var};
use crate::utils::settings::SettingsJson;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DangerousSettings {
    pub shell_settings: Vec<(String, String)>,
    pub env_vars: Vec<(String, String)>,
    pub has_hooks: bool,
    pub hooks: Option<serde_json::Value>,
}

fn hooks_key_count(value: &serde_json::Value) -> usize {
    if let Some(object) = value.as_object() {
        object.len()
    } else if let Some(array) = value.as_array() {
        array.len()
    } else {
        0
    }
}

fn shell_setting_value(settings: &SettingsJson, key: &str) -> Option<String> {
    match key {
        "apiKeyHelper" => settings.api_key_helper.clone(),
        "awsAuthRefresh" => settings.aws_auth_refresh.clone(),
        "awsCredentialExport" => settings.aws_credential_export.clone(),
        "gcpAuthRefresh" => settings.gcp_auth_refresh.clone(),
        "otelHeadersHelper" => settings.otel_headers_helper.clone(),
        // Maps the official `typeof value === 'string'` guard: Rust models
        // `statusLine` as an object, so it is not included in shellSettings by
        // this helper. Hook/status-line trust gates are handled elsewhere.
        "statusLine" => None,
        _ => None,
    }
    .filter(|value| !value.is_empty())
}

/// Maps to: CC `utils.ts#extractDangerousSettings(...)`.
pub fn extract_dangerous_settings(settings: Option<&SettingsJson>) -> DangerousSettings {
    let Some(settings) = settings else {
        return DangerousSettings::default();
    };

    let shell_settings = DANGEROUS_SHELL_SETTINGS
        .iter()
        .filter_map(|key| {
            shell_setting_value(settings, key).map(|value| ((*key).to_string(), value))
        })
        .collect::<Vec<_>>();

    let mut env_vars = settings
        .env
        .as_ref()
        .map(|env| {
            env.iter()
                .filter(|(key, value)| !value.is_empty() && !is_safe_managed_env_var(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    env_vars.sort_by(|a, b| a.0.cmp(&b.0));

    let has_hooks = settings
        .hooks
        .as_ref()
        .is_some_and(|hooks| hooks.is_object() && hooks_key_count(hooks) > 0);

    DangerousSettings {
        shell_settings,
        env_vars,
        has_hooks,
        hooks: has_hooks.then(|| settings.hooks.clone()).flatten(),
    }
}

/// Maps to: CC `utils.ts#hasDangerousSettings(...)`.
pub fn has_dangerous_settings(dangerous: &DangerousSettings) -> bool {
    !dangerous.shell_settings.is_empty() || !dangerous.env_vars.is_empty() || dangerous.has_hooks
}

/// Maps to: CC `utils.ts#hasDangerousSettingsChanged(...)`.
pub fn has_dangerous_settings_changed(
    old_settings: Option<&SettingsJson>,
    new_settings: Option<&SettingsJson>,
) -> bool {
    let old_dangerous = extract_dangerous_settings(old_settings);
    let new_dangerous = extract_dangerous_settings(new_settings);

    if !has_dangerous_settings(&new_dangerous) {
        return false;
    }
    if !has_dangerous_settings(&old_dangerous) {
        return true;
    }

    old_dangerous.shell_settings != new_dangerous.shell_settings
        || old_dangerous.env_vars != new_dangerous.env_vars
        || old_dangerous.hooks != new_dangerous.hooks
}

/// Maps to: CC `utils.ts#formatDangerousSettingsList(...)`.
pub fn format_dangerous_settings_list(dangerous: &DangerousSettings) -> Vec<String> {
    let mut items = Vec::new();
    items.extend(dangerous.shell_settings.iter().map(|(key, _)| key.clone()));
    items.extend(dangerous.env_vars.iter().map(|(key, _)| key.clone()));
    if dangerous.has_hooks {
        items.push("hooks".to_string());
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_dangerous_settings_matches_official_shell_env_and_hooks_rules() {
        let settings = SettingsJson {
            api_key_helper: Some("helper".to_string()),
            aws_auth_refresh: Some("".to_string()),
            env: Some(std::sync::Arc::new(indexmap::IndexMap::from([(
                "ANTHROPIC_BASE_URL".to_string(),
                "https://evil".to_string(),
            )]))),
            hooks: Some(serde_json::json!({ "PreToolUse": [{ "command": "echo" }] })),
            status_line: Some(crate::utils::settings::types::StatusLineSettings {
                kind: Some("command".to_string()),
                command: "echo status".to_string(),
                padding: None,
            }),
            ..SettingsJson::default()
        };

        let dangerous = extract_dangerous_settings(Some(&settings));
        assert_eq!(dangerous.shell_settings[0].0, "apiKeyHelper");
        assert!(
            !dangerous
                .shell_settings
                .iter()
                .any(|(key, _)| key == "statusLine")
        );
        assert_eq!(dangerous.env_vars[0].0, "ANTHROPIC_BASE_URL");
        assert!(dangerous.has_hooks);
        assert_eq!(
            format_dangerous_settings_list(&dangerous),
            vec!["apiKeyHelper", "ANTHROPIC_BASE_URL", "hooks"]
        );
    }

    #[test]
    fn dangerous_settings_changed_matches_official_prompt_gate() {
        let old = SettingsJson::default();
        let new = SettingsJson {
            gcp_auth_refresh: Some("gcloud auth".to_string()),
            ..SettingsJson::default()
        };
        assert!(has_dangerous_settings_changed(Some(&old), Some(&new)));
        assert!(!has_dangerous_settings_changed(Some(&new), Some(&new)));
        assert!(!has_dangerous_settings_changed(Some(&new), Some(&old)));
    }
}
