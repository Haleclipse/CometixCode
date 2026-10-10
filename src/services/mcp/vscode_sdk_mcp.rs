//! VSCode SDK MCP integration.
//! Maps to: CC `services/mcp/vscodeSdkMcp.ts`.

use serde_json::Value;
use std::collections::BTreeMap;

pub const VSCODE_MCP_SERVER_NAME: &str = "claude-vscode";
pub const LOG_EVENT_METHOD: &str = "log_event";
pub const FILE_UPDATED_METHOD: &str = "file_updated";
pub const EXPERIMENT_GATES_METHOD: &str = "experiment_gates";

/// Maps to: CC `vscodeSdkMcp.ts` local `AutoModeEnabledState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoModeEnabledState {
    Enabled,
    Disabled,
    OptIn,
}

impl AutoModeEnabledState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::OptIn => "opt-in",
        }
    }
}

/// Maps to: CC `vscodeSdkMcp.ts#readAutoModeEnabledState` value validation.
pub fn read_auto_mode_enabled_state(value: Option<&str>) -> Option<AutoModeEnabledState> {
    match value {
        Some("enabled") => Some(AutoModeEnabledState::Enabled),
        Some("disabled") => Some(AutoModeEnabledState::Disabled),
        Some("opt-in") => Some(AutoModeEnabledState::OptIn),
        _ => None,
    }
}

/// Maps to: CC `vscodeSdkMcp.ts` `logEvent(`tengu_vscode_${eventName}`, ...)`.
pub fn vscode_log_event_name(event_name: &str) -> String {
    format!("tengu_vscode_{event_name}")
}

/// Maps to: CC `setupVscodeSdkMcp(...)` `gates` payload.
/// Cometix does not implement telemetry/GrowthBook delivery; unknown VSCode
/// experiment gates fail closed as `false`, while the tri-state auto-mode value
/// is only included when already known.
pub fn vscode_experiment_gates(
    auto_mode_state: Option<AutoModeEnabledState>,
) -> BTreeMap<String, Value> {
    let mut gates = BTreeMap::from([
        ("tengu_vscode_review_upsell".to_string(), Value::Bool(false)),
        ("tengu_vscode_onboarding".to_string(), Value::Bool(false)),
        ("tengu_quiet_fern".to_string(), Value::Bool(false)),
        ("tengu_vscode_cc_auth".to_string(), Value::Bool(false)),
    ]);
    if let Some(state) = auto_mode_state {
        gates.insert(
            "tengu_auto_mode_state".to_string(),
            Value::String(state.as_str().to_string()),
        );
    }
    gates
}

pub fn file_updated_params(
    file_path: &str,
    old_content: Option<&str>,
    new_content: Option<&str>,
) -> Value {
    // Maps to: CC `notifyVscodeFileUpdated(...)` notification params.
    serde_json::json!({
        "filePath": file_path,
        "oldContent": old_content,
        "newContent": new_content,
    })
}

pub fn should_notify_vscode_file_updated_for_audience(
    audience: crate::utils::build_profile::BuildAudience,
    has_vscode_client: bool,
) -> bool {
    // Maps to CC's internal-distribution + connected-client guard.
    crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Api,
    ) && has_vscode_client
}

pub fn handle_vscode_log_event_notification(
    server_name: &str,
    notification: &rmcp::model::CustomNotification,
) -> bool {
    // Maps to: CC `LogEventNotificationSchema` handler registered by
    // `setupVscodeSdkMcp(...)`. Telemetry is intentionally not emitted in
    // Cometix; we preserve the event tag for debugging/auditing only.
    if server_name != VSCODE_MCP_SERVER_NAME || notification.method != LOG_EVENT_METHOD {
        return false;
    }
    let Some(params) = notification.params.as_ref() else {
        return true;
    };
    let Some(event_name) = params.get("eventName").and_then(Value::as_str) else {
        return true;
    };
    tracing::debug!(
        event = %vscode_log_event_name(event_name),
        data = ?params.get("eventData"),
        "VSCode MCP log_event notification received"
    );
    true
}

pub async fn setup_vscode_sdk_mcp(
    sdk_clients: &[crate::services::mcp::types::McpServerSnapshot],
) -> bool {
    // Maps to: CC `services/mcp/vscodeSdkMcp.ts#setupVscodeSdkMcp`:
    // store/recognize the connected `claude-vscode` SDK MCP client and send
    // the initial `experiment_gates` notification. The log_event handler is
    // installed by `client.rs#on_custom_notification` for this internal server.
    let has_connected_vscode = sdk_clients.iter().any(|server| {
        server.client.name == VSCODE_MCP_SERVER_NAME
            && server.client.status
                == crate::services::mcp::types::McpServerConnectionType::Connected
    });
    if !has_connected_vscode {
        return false;
    }
    let gates = vscode_experiment_gates(None);
    crate::services::mcp::client::send_custom_notification_to_connected_client(
        VSCODE_MCP_SERVER_NAME,
        EXPERIMENT_GATES_METHOD,
        serde_json::json!({ "gates": gates }),
    )
    .await
    .is_ok()
}

pub async fn notify_vscode_file_updated(
    file_path: &str,
    old_content: Option<&str>,
    new_content: Option<&str>,
) -> bool {
    if !should_notify_vscode_file_updated_for_audience(
        crate::utils::build_profile::build_audience(),
        crate::services::mcp::client::is_connected_mcp_client(VSCODE_MCP_SERVER_NAME).await,
    ) {
        return false;
    }
    let params = file_updated_params(file_path, old_content, new_content);
    match crate::services::mcp::client::send_custom_notification_to_connected_client(
        VSCODE_MCP_SERVER_NAME,
        FILE_UPDATED_METHOD,
        params,
    )
    .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(error = %error, "[VSCode] Failed to send file_updated notification");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mode_state_validation_matches_official_tristate() {
        assert_eq!(
            read_auto_mode_enabled_state(Some("enabled")),
            Some(AutoModeEnabledState::Enabled)
        );
        assert_eq!(
            read_auto_mode_enabled_state(Some("disabled")),
            Some(AutoModeEnabledState::Disabled)
        );
        assert_eq!(
            read_auto_mode_enabled_state(Some("opt-in")),
            Some(AutoModeEnabledState::OptIn)
        );
        assert_eq!(read_auto_mode_enabled_state(Some("other")), None);
        assert_eq!(read_auto_mode_enabled_state(None), None);
    }

    #[test]
    fn vscode_experiment_gates_omit_unknown_auto_mode_and_fail_closed() {
        let gates = vscode_experiment_gates(None);
        assert_eq!(
            gates.get("tengu_vscode_review_upsell"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            gates.get("tengu_vscode_onboarding"),
            Some(&Value::Bool(false))
        );
        assert_eq!(gates.get("tengu_quiet_fern"), Some(&Value::Bool(false)));
        assert_eq!(gates.get("tengu_vscode_cc_auth"), Some(&Value::Bool(false)));
        assert!(!gates.contains_key("tengu_auto_mode_state"));

        let gates = vscode_experiment_gates(Some(AutoModeEnabledState::OptIn));
        assert_eq!(
            gates.get("tengu_auto_mode_state"),
            Some(&Value::String("opt-in".to_string()))
        );
    }

    #[test]
    fn file_updated_notification_gate_matches_internal_and_client_guard() {
        use crate::utils::build_profile::BuildAudience;

        assert!(should_notify_vscode_file_updated_for_audience(
            BuildAudience::AnthropicInternal,
            true
        ));
        assert!(!should_notify_vscode_file_updated_for_audience(
            BuildAudience::AnthropicInternal,
            false
        ));
        assert!(!should_notify_vscode_file_updated_for_audience(
            BuildAudience::External,
            true
        ));
        assert_eq!(
            file_updated_params("src/lib.rs", Some("old"), None),
            serde_json::json!({
                "filePath": "src/lib.rs",
                "oldContent": "old",
                "newContent": null,
            })
        );
    }

    #[test]
    fn vscode_log_event_handler_filters_server_and_method() {
        let notification = rmcp::model::CustomNotification::new(
            LOG_EVENT_METHOD,
            Some(serde_json::json!({
                "eventName": "opened",
                "eventData": { "ok": true }
            })),
        );
        assert!(handle_vscode_log_event_notification(
            VSCODE_MCP_SERVER_NAME,
            &notification
        ));
        assert!(!handle_vscode_log_event_notification(
            "other",
            &notification
        ));
        let other = rmcp::model::CustomNotification::new("other", None);
        assert!(!handle_vscode_log_event_notification(
            VSCODE_MCP_SERVER_NAME,
            &other
        ));
        assert_eq!(vscode_log_event_name("opened"), "tengu_vscode_opened");
    }
}
