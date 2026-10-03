//! MCP `.mcp.json` approval service helpers.
//! Maps to: CC `services/mcpServerApproval.tsx`,
//! `components/MCPServerApprovalDialog.tsx`, and
//! `components/MCPServerMultiselectDialog.tsx` settings-update paths.
//!
//! The interactive rendering remains in `components/*Dialog`; this module owns
//! the official pure state transitions so future wiring can keep settings
//! mutation out of render components.

use crate::services::mcp::types::ScopedMcpServerConfig;
use crate::services::mcp::utils::{
    ProjectMcpServerStatus, get_project_mcp_server_status_from_settings,
};
use crate::utils::settings::SettingsJson;
use crate::utils::settings::constants::SettingSource;
use crate::utils::settings::{get_settings_file_path_for_source, get_settings_for_source};
use serde_json::Value;
#[cfg(test)]
use std::collections::BTreeMap;

/// Maps to: CC `MCPServerApprovalDialog.tsx` select values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpjsonServerApprovalChoice {
    YesAll,
    Yes,
    No,
}

fn push_unique(list: &mut Option<Vec<String>>, value: impl Into<String>) {
    let value = value.into();
    let entries = list.get_or_insert_with(Vec::new);
    if !entries.iter().any(|entry| entry == &value) {
        entries.push(value);
    }
}

/// Maps to: CC `handleMcpjsonServerApprovals(...)` pending-server discovery:
/// `getMcpConfigsByScope('project')` filtered by
/// `getProjectMcpServerStatus(serverName) === 'pending'`.
pub fn pending_project_mcp_server_names_from_configs(
    project_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    settings: &SettingsJson,
    project_settings_enabled: bool,
    has_skip_dangerous_mode_permission_prompt: bool,
    is_non_interactive_session: bool,
) -> Vec<String> {
    project_servers
        .keys()
        .filter(|server_name| {
            get_project_mcp_server_status_from_settings(
                server_name,
                settings,
                project_settings_enabled,
                has_skip_dangerous_mode_permission_prompt,
                is_non_interactive_session,
            ) == ProjectMcpServerStatus::Pending
        })
        .cloned()
        .collect()
}

/// Maps to: CC `MCPServerApprovalDialog.tsx#onChange` settings updates.
pub fn apply_single_mcpjson_server_approval_choice(
    settings: &mut SettingsJson,
    server_name: &str,
    choice: McpjsonServerApprovalChoice,
) {
    match choice {
        McpjsonServerApprovalChoice::Yes | McpjsonServerApprovalChoice::YesAll => {
            push_unique(
                &mut settings.enabled_mcpjson_servers,
                server_name.to_string(),
            );
            if choice == McpjsonServerApprovalChoice::YesAll {
                settings.enable_all_project_mcp_servers = Some(true);
            }
        }
        McpjsonServerApprovalChoice::No => {
            push_unique(
                &mut settings.disabled_mcpjson_servers,
                server_name.to_string(),
            );
        }
    }
}

/// Maps to: CC `MCPServerMultiselectDialog.tsx#onSubmit` settings updates.
pub fn apply_multi_mcpjson_server_approval_result(
    settings: &mut SettingsJson,
    approved_servers: &[String],
    rejected_servers: &[String],
) {
    for server in approved_servers {
        push_unique(&mut settings.enabled_mcpjson_servers, server.clone());
    }
    for server in rejected_servers {
        push_unique(&mut settings.disabled_mcpjson_servers, server.clone());
    }
}

/// Maps to: CC `MCPServerMultiselectDialog.tsx#handleEscRejectAll`.
pub fn apply_mcpjson_reject_all(settings: &mut SettingsJson, server_names: &[String]) {
    for server in server_names {
        push_unique(&mut settings.disabled_mcpjson_servers, server.clone());
    }
}

fn local_settings_path_for_mcpjson_approval() -> anyhow::Result<std::path::PathBuf> {
    get_settings_file_path_for_source(SettingSource::Local)
        .ok_or_else(|| anyhow::anyhow!("local settings path is unavailable"))
}

fn read_local_settings_json_value() -> anyhow::Result<Value> {
    let path = local_settings_path_for_mcpjson_approval()?;
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(serde_json::from_str::<Value>(&content)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(error) => Err(error.into()),
    }
}

fn set_optional_array_field(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    values: &Option<Vec<String>>,
) {
    if let Some(values) = values {
        object.insert(
            key.to_string(),
            Value::Array(values.iter().cloned().map(Value::String).collect()),
        );
    }
}

fn write_local_mcpjson_approval_settings(settings: &SettingsJson) -> anyhow::Result<()> {
    // Maps to: CC `updateSettingsForSource('localSettings', ...)` in
    // MCPServerApprovalDialog / MCPServerMultiselectDialog.
    let mut value = read_local_settings_json_value()?;
    if !value.is_object() {
        value = serde_json::json!({});
    }
    let object = value.as_object_mut().expect("object checked above");
    set_optional_array_field(
        object,
        "enabledMcpjsonServers",
        &settings.enabled_mcpjson_servers,
    );
    set_optional_array_field(
        object,
        "disabledMcpjsonServers",
        &settings.disabled_mcpjson_servers,
    );
    if let Some(enable_all) = settings.enable_all_project_mcp_servers {
        object.insert(
            "enableAllProjectMcpServers".to_string(),
            Value::Bool(enable_all),
        );
    }

    let path = local_settings_path_for_mcpjson_approval()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&value)?)?;
    // CC reaches this through `updateSettingsForSource`, which invalidates the
    // session cache as part of the write (`settings.ts:505-506`). This function
    // writes the file directly — it needs the same fields preserved and a
    // different shape than the generic updater gives — so the invalidation has
    // to be explicit. Without it every later `get_settings_*` read returns the
    // pre-approval merge, and the approval silently does nothing until the
    // process restarts.
    crate::utils::settings::settings_cache::reset_settings_cache();
    Ok(())
}

fn load_local_settings_for_mcpjson_approval() -> SettingsJson {
    get_settings_for_source(SettingSource::Local).unwrap_or_default()
}

/// Maps to: CC single-server approval dialog writing local settings.
pub fn apply_single_mcpjson_server_approval_choice_to_local_settings(
    server_name: &str,
    choice: McpjsonServerApprovalChoice,
) -> anyhow::Result<SettingsJson> {
    let mut settings = load_local_settings_for_mcpjson_approval();
    apply_single_mcpjson_server_approval_choice(&mut settings, server_name, choice);
    write_local_mcpjson_approval_settings(&settings)?;
    Ok(settings)
}

/// Maps to: CC multiselect approval dialog writing local settings.
pub fn apply_multi_mcpjson_server_approval_result_to_local_settings(
    approved_servers: &[String],
    rejected_servers: &[String],
) -> anyhow::Result<SettingsJson> {
    let mut settings = load_local_settings_for_mcpjson_approval();
    apply_multi_mcpjson_server_approval_result(&mut settings, approved_servers, rejected_servers);
    write_local_mcpjson_approval_settings(&settings)?;
    Ok(settings)
}

/// Maps to: CC multiselect Escape path writing all servers to disabled list.
pub fn apply_mcpjson_reject_all_to_local_settings(
    server_names: &[String],
) -> anyhow::Result<SettingsJson> {
    let mut settings = load_local_settings_for_mcpjson_approval();
    apply_mcpjson_reject_all(&mut settings, server_names);
    write_local_mcpjson_approval_settings(&settings)?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mcp::types::{ConfigScope, Transport};
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn project_config(command: &str) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            name: None,
            transport: Transport::Stdio,
            command: Some(command.to_string()),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            headers_helper: None,
            scope: ConfigScope::Project,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: None,
            plugin_source: None,
        }
    }

    #[test]
    fn pending_project_mcp_server_names_filters_by_official_status() {
        let mut servers = indexmap::IndexMap::new();
        servers.insert("docs".to_string(), project_config("docs-mcp"));
        servers.insert("blocked".to_string(), project_config("blocked-mcp"));
        servers.insert("chosen".to_string(), project_config("chosen-mcp"));

        let settings = SettingsJson {
            enabled_mcpjson_servers: Some(vec!["chosen".to_string()]),
            disabled_mcpjson_servers: Some(vec!["blocked".to_string()]),
            ..SettingsJson::default()
        };

        assert_eq!(
            pending_project_mcp_server_names_from_configs(&servers, &settings, true, false, false,),
            vec!["docs".to_string()]
        );

        let settings = SettingsJson {
            enable_all_project_mcp_servers: Some(true),
            ..SettingsJson::default()
        };
        assert!(
            pending_project_mcp_server_names_from_configs(&servers, &settings, true, false, false,)
                .is_empty()
        );
    }

    #[test]
    fn single_mcpjson_approval_choice_updates_settings_like_official_dialog() {
        let mut settings = SettingsJson::default();
        apply_single_mcpjson_server_approval_choice(
            &mut settings,
            "docs",
            McpjsonServerApprovalChoice::Yes,
        );
        apply_single_mcpjson_server_approval_choice(
            &mut settings,
            "docs",
            McpjsonServerApprovalChoice::Yes,
        );
        assert_eq!(
            settings.enabled_mcpjson_servers,
            Some(vec!["docs".to_string()])
        );
        assert_eq!(settings.enable_all_project_mcp_servers, None);

        apply_single_mcpjson_server_approval_choice(
            &mut settings,
            "all-docs",
            McpjsonServerApprovalChoice::YesAll,
        );
        assert_eq!(
            settings.enabled_mcpjson_servers,
            Some(vec!["docs".to_string(), "all-docs".to_string()])
        );
        assert_eq!(settings.enable_all_project_mcp_servers, Some(true));

        apply_single_mcpjson_server_approval_choice(
            &mut settings,
            "blocked",
            McpjsonServerApprovalChoice::No,
        );
        assert_eq!(
            settings.disabled_mcpjson_servers,
            Some(vec!["blocked".to_string()])
        );
    }

    #[test]
    fn local_settings_write_preserves_unrelated_fields_like_update_settings_for_source() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-mcp-approval-write-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(temp_dir.join(".claude")).unwrap();
        std::fs::write(
            temp_dir.join(".claude/settings.local.json"),
            serde_json::json!({
                "language": "en",
                "enabledMcpjsonServers": ["existing"]
            })
            .to_string(),
        )
        .unwrap();
        // `localSettings` is rooted at `get_original_cwd()`, not the process
        // cwd (`utils/settings/mod.rs:54-56`). Moving only the process cwd let
        // this test read and REWRITE the repository's own
        // `.claude/settings.local.json` whenever `ORIGINAL_CWD` had already
        // been initialised by an earlier test.
        struct ProjectRootGuard {
            previous_cwd: std::path::PathBuf,
            previous_original_cwd: std::path::PathBuf,
        }
        impl Drop for ProjectRootGuard {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.previous_cwd);
                crate::bootstrap::state::set_original_cwd(&self.previous_original_cwd);
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }

        let _root = ProjectRootGuard {
            previous_cwd: std::env::current_dir().unwrap(),
            previous_original_cwd: crate::bootstrap::state::get_original_cwd(),
        };
        std::env::set_current_dir(&temp_dir).unwrap();
        crate::bootstrap::state::set_original_cwd(&temp_dir);
        crate::utils::settings::settings_cache::reset_settings_cache();

        let updated = apply_single_mcpjson_server_approval_choice_to_local_settings(
            "docs",
            McpjsonServerApprovalChoice::YesAll,
        )
        .unwrap();

        let written: Value = serde_json::from_str(
            &std::fs::read_to_string(temp_dir.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["language"], "en");
        assert_eq!(
            written["enabledMcpjsonServers"],
            serde_json::json!(["existing", "docs"])
        );
        assert_eq!(written["enableAllProjectMcpServers"], true);
        assert_eq!(
            updated.enabled_mcpjson_servers,
            Some(vec!["existing".to_string(), "docs".to_string()])
        );

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn multi_mcpjson_approval_result_and_reject_all_dedupe_like_official_set_merge() {
        let mut settings = SettingsJson {
            enabled_mcpjson_servers: Some(vec!["existing".to_string()]),
            disabled_mcpjson_servers: Some(vec!["old-block".to_string()]),
            ..SettingsJson::default()
        };

        apply_multi_mcpjson_server_approval_result(
            &mut settings,
            &["existing".to_string(), "docs".to_string()],
            &["blocked".to_string(), "old-block".to_string()],
        );
        assert_eq!(
            settings.enabled_mcpjson_servers,
            Some(vec!["existing".to_string(), "docs".to_string()])
        );
        assert_eq!(
            settings.disabled_mcpjson_servers,
            Some(vec!["old-block".to_string(), "blocked".to_string()])
        );

        apply_mcpjson_reject_all(&mut settings, &["blocked".to_string(), "other".to_string()]);
        assert_eq!(
            settings.disabled_mcpjson_servers,
            Some(vec![
                "old-block".to_string(),
                "blocked".to_string(),
                "other".to_string(),
            ])
        );
    }
}
