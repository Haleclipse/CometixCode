//! StatusLine command execution.
//!
//! Maps to official `utils/hooks.ts#executeStatusLineCommand`. JSON organization
//! lives in [`crate::components::status_line::build_status_line_command_input`].

use crate::types::status_line::StatusLineCommandInput;
use crate::utils::settings::SettingsJson;
use std::time::Duration;

pub const STATUS_LINE_TIMEOUT_MS: u64 = 5_000;

/// Resolve the configured statusLine command under official trust / managed
/// hooks gates. Maps to the settings lookup inside CC `executeStatusLineCommand`.
pub fn status_line_command_from_settings(settings: &SettingsJson) -> Option<String> {
    status_line_command_from_settings_with_policy(settings, None, true)
}

pub fn status_line_command_from_settings_with_trust(
    settings: &SettingsJson,
    workspace_trusted: bool,
) -> Option<String> {
    status_line_command_from_settings_with_policy(settings, None, workspace_trusted)
}

pub fn status_line_command_from_settings_with_policy(
    settings: &SettingsJson,
    policy_settings: Option<&SettingsJson>,
    workspace_trusted: bool,
) -> Option<String> {
    if !workspace_trusted {
        // Maps to CC: `Skipping StatusLine command execution - workspace trust not accepted`
        crate::utils::debug::log_for_debugging(
            "Skipping StatusLine command execution - workspace trust not accepted",
        );
        return None;
    }

    // Official `shouldDisableAllHooksIncludingManaged()`: only policy settings
    // can disable managed hooks/statusLine commands.
    if policy_settings.is_some_and(|policy| policy.disable_all_hooks == Some(true)) {
        return None;
    }

    let managed_only = policy_settings
        .is_some_and(|policy| policy.allow_managed_hooks_only == Some(true))
        || (settings.disable_all_hooks == Some(true)
            && !policy_settings.is_some_and(|policy| policy.disable_all_hooks == Some(true)));
    let status_line_source = if managed_only {
        policy_settings?
    } else {
        settings
    };

    status_line_source
        .status_line
        .as_ref()
        .filter(|status_line| status_line.is_command_type())
        .map(|status_line| status_line.command.clone())
}

/// Execute the status line command with the organized JSON stdin payload.
///
/// Maps to: CC `executeStatusLineCommand(statusLineInput, signal, timeoutMs, logResult)`.
pub async fn execute_status_line_command(
    status_line_input: &StatusLineCommandInput,
    workspace_trusted: bool,
    policy_settings: Option<&SettingsJson>,
    log_result: bool,
) -> Option<String> {
    let settings = crate::utils::settings::get_initial_settings();
    let command = status_line_command_from_settings_with_policy(
        &settings,
        policy_settings,
        workspace_trusted,
    )?;
    execute_status_line_command_with_command(command, status_line_input, log_result).await
}

/// Lower-level executor when the caller already resolved the command string
/// (e.g. main startup / tests).
pub async fn execute_status_line_command_with_command(
    command: String,
    status_line_input: &StatusLineCommandInput,
    log_result: bool,
) -> Option<String> {
    if command.trim().is_empty() {
        return None;
    }

    let input_json = serde_json::to_string(status_line_input).ok()?;
    let result = super::exec::exec_command_hook(
        &command,
        &input_json,
        Duration::from_millis(STATUS_LINE_TIMEOUT_MS),
        vec![],
        None,
        None,
        None,
    )
    .await;

    if log_result {
        // Maps to CC StatusLine completion logs.
        crate::utils::debug::log_for_debugging(&format!(
            "StatusLine [{command}] completed with status {}",
            result.status
        ));
    }
    if result.aborted || result.status != 0 {
        return None;
    }

    normalize_status_line_stdout(&result.stdout)
}

fn normalize_status_line_stdout(stdout: &str) -> Option<String> {
    let output = stdout
        .trim()
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            (!line.is_empty()).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n");

    (!output.is_empty()).then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::status_line::build_status_line_command_input;
    use crate::types::permissions::PermissionMode;

    #[test]
    fn status_line_command_from_settings_respects_official_disable_all_hooks_gate() {
        let settings = SettingsJson {
            status_line: Some(crate::utils::settings::types::StatusLineSettings {
                kind: Some("command".to_string()),
                command: "printf status".to_string(),
                padding: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            status_line_command_from_settings(&settings),
            Some("printf status".to_string())
        );

        let disabled = SettingsJson {
            disable_all_hooks: Some(true),
            ..settings
        };
        assert_eq!(status_line_command_from_settings(&disabled), None);
    }

    #[test]
    fn status_line_command_from_settings_respects_official_workspace_trust_gate() {
        let settings = SettingsJson {
            status_line: Some(crate::utils::settings::types::StatusLineSettings {
                kind: Some("command".to_string()),
                command: "printf status".to_string(),
                padding: None,
            }),
            ..Default::default()
        };

        assert_eq!(
            status_line_command_from_settings_with_trust(&settings, true),
            Some("printf status".to_string())
        );
        assert_eq!(
            status_line_command_from_settings_with_trust(&settings, false),
            None
        );
    }

    #[test]
    fn status_line_command_from_settings_respects_official_managed_only_source_filter() {
        let policy = SettingsJson {
            status_line: Some(crate::utils::settings::types::StatusLineSettings {
                kind: Some("command".to_string()),
                command: "printf managed".to_string(),
                padding: None,
            }),
            ..Default::default()
        };
        let non_managed_disabled = SettingsJson {
            disable_all_hooks: Some(true),
            status_line: Some(crate::utils::settings::types::StatusLineSettings {
                kind: Some("command".to_string()),
                command: "printf user".to_string(),
                padding: None,
            }),
            ..Default::default()
        };

        assert_eq!(
            status_line_command_from_settings_with_policy(
                &non_managed_disabled,
                Some(&policy),
                true
            ),
            Some("printf managed".to_string())
        );

        let policy_disabled = SettingsJson {
            disable_all_hooks: Some(true),
            ..policy.clone()
        };
        assert_eq!(
            status_line_command_from_settings_with_policy(
                &non_managed_disabled,
                Some(&policy_disabled),
                true
            ),
            None
        );

        let policy_managed_only_without_status_line = SettingsJson {
            allow_managed_hooks_only: Some(true),
            ..Default::default()
        };
        assert_eq!(
            status_line_command_from_settings_with_policy(
                &non_managed_disabled,
                Some(&policy_managed_only_without_status_line),
                true
            ),
            None
        );
    }

    #[test]
    fn status_line_stdout_normalization_matches_official_shape() {
        assert_eq!(
            normalize_status_line_stdout("  alpha  \n\n beta \n"),
            Some("alpha\nbeta".to_string())
        );
        assert_eq!(normalize_status_line_stdout(" \n\n "), None);
    }

    #[test]
    fn status_line_input_serializes_official_shape_without_placeholder_zeros_for_usage() {
        let settings = SettingsJson {
            output_style: Some("explanatory".to_string()),
            ..Default::default()
        };
        let model = "claude-sonnet-4-6".to_string();
        let input = build_status_line_command_input(
            PermissionMode::AcceptEdits,
            false,
            &settings,
            &[],
            &[],
            &model,
            None,
        );
        let value = serde_json::to_value(&input).expect("serialize");

        assert!(value.get("context").is_none());
        assert!(value["session_id"].as_str().is_some());
        assert!(value["transcript_path"].as_str().is_some());
        assert_eq!(value["output_style"]["name"], "explanatory");
        assert!(value["context_window"]["current_usage"].is_null());
        assert!(value["context_window"]["used_percentage"].is_null());
        assert!(value["context_window"]["remaining_percentage"].is_null());
        assert!(value.get("nyan").is_none());
        assert!(value.get("permission_mode").is_none());
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn status_line_command_receives_organized_json_on_stdin() {
        let input = build_status_line_command_input(
            PermissionMode::Default,
            false,
            &SettingsJson::default(),
            &[],
            &[],
            "claude-sonnet-4-6",
            None,
        );
        let output = execute_status_line_command_with_command(
            "grep -q '\"model\"' && printf ' status ok \\n\\n'".to_string(),
            &input,
            true,
        )
        .await;

        assert_eq!(output, Some("status ok".to_string()));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn status_line_command_can_ignore_stdin() {
        let input = build_status_line_command_input(
            PermissionMode::Default,
            false,
            &SettingsJson::default(),
            &[],
            &[],
            "claude-sonnet-4-6",
            None,
        );
        let output = execute_status_line_command_with_command(
            "printf 'static status\\n'".to_string(),
            &input,
            false,
        )
        .await;

        assert_eq!(output, Some("static status".to_string()));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn status_line_command_failure_returns_none() {
        let input = build_status_line_command_input(
            PermissionMode::Default,
            false,
            &SettingsJson::default(),
            &[],
            &[],
            "claude-sonnet-4-6",
            None,
        );
        assert_eq!(
            execute_status_line_command_with_command("exit 7".to_string(), &input, false).await,
            None
        );
    }
}
