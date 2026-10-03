//! Maps to: CC `services/mcp/headersHelper.ts`.
//!
//! This module owns dynamic MCP header helper execution and static/dynamic
//! header merging. Transport/client setup calls into this service module rather
//! than embedding `headersHelper` process execution in `client.rs`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;

use super::types::{ConfigScope, ScopedMcpServerConfig};

/// Maps to: CC `services/mcp/headersHelper.ts#isMcpServerFromProjectOrLocalSettings`.
fn is_mcp_server_from_project_or_local_settings(config: &ScopedMcpServerConfig) -> bool {
    matches!(config.scope, ConfigScope::Project | ConfigScope::Local)
}

/// Maps to: CC `services/mcp/headersHelper.ts#getMcpHeadersFromHelper`.
pub async fn get_mcp_headers_from_helper(
    server_name: &str,
    config: &ScopedMcpServerConfig,
) -> Option<BTreeMap<String, String>> {
    let helper = config
        .headers_helper
        .as_deref()
        .filter(|helper| !helper.trim().is_empty())?;

    if is_mcp_server_from_project_or_local_settings(config)
        && !crate::bootstrap::state::get_is_non_interactive_session()
        && !crate::utils::config::check_has_trust_dialog_accepted()
    {
        tracing::error!(
            server = server_name,
            "Security: headersHelper executed before workspace trust is confirmed"
        );
        return None;
    }

    let mut command = if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", helper]);
        command
    } else {
        // CC execa `shell: true` (headersHelper.ts:62): `/bin/sh` off Windows.
        let mut command = Command::new("/bin/sh");
        command.args(["-c", helper]);
        command
    };
    // CC headersHelper.ts:66-70: `{ ...process.env, CLAUDE_CODE_MCP_SERVER_NAME,
    // CLAUDE_CODE_MCP_SERVER_URL: config.url }` — process.env, not
    // subprocessEnv(); an undefined url drops the inherited key.
    crate::utils::subprocess_env::apply_process_env(&mut command);
    command.env("CLAUDE_CODE_MCP_SERVER_NAME", server_name);
    match config.url.as_deref() {
        Some(url) => command.env("CLAUDE_CODE_MCP_SERVER_URL", url),
        None => command.env_remove("CLAUDE_CODE_MCP_SERVER_URL"),
    };

    let output = match tokio::time::timeout(Duration::from_secs(10), command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            tracing::warn!(server = server_name, error = %error, "failed to execute MCP headersHelper");
            return None;
        }
        Err(_) => {
            tracing::warn!(server = server_name, "MCP headersHelper timed out");
            return None;
        }
    };

    if !output.status.success() || output.stdout.is_empty() {
        tracing::warn!(server = server_name, status = ?output.status, "MCP headersHelper returned no usable headers");
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let value = match serde_json::from_str::<Value>(&stdout) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(server = server_name, error = %error, "MCP headersHelper returned invalid JSON");
            return None;
        }
    };

    let Some(object) = value.as_object() else {
        tracing::warn!(
            server = server_name,
            "MCP headersHelper did not return an object"
        );
        return None;
    };

    let mut headers = BTreeMap::new();
    for (key, value) in object {
        let Some(value) = value.as_str() else {
            tracing::warn!(
                server = server_name,
                header = key,
                value_type = value_type_name(value),
                "MCP headersHelper returned non-string header value"
            );
            return None;
        };
        headers.insert(key.clone(), value.to_string());
    }

    tracing::debug!(
        server = server_name,
        count = headers.len(),
        "Successfully retrieved headers from MCP headersHelper"
    );
    Some(headers)
}

/// Maps to: CC `services/mcp/headersHelper.ts#getMcpServerHeaders`.
pub async fn get_mcp_server_headers(
    server_name: &str,
    config: &ScopedMcpServerConfig,
) -> BTreeMap<String, String> {
    let mut headers = config.headers.clone();
    if let Some(dynamic_headers) = get_mcp_headers_from_helper(server_name, config).await {
        headers.extend(dynamic_headers);
    }
    headers
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mcp::types::{ConfigScope, Transport};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn config(scope: ConfigScope, helper: Option<String>) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            name: None,
            scope,
            transport: Transport::Http,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some("https://example.com/mcp".to_string()),
            headers: BTreeMap::from([
                ("X-Static".to_string(), "static".to_string()),
                ("X-Override".to_string(), "static".to_string()),
            ]),
            headers_helper: helper,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: None,
            plugin_source: None,
        }
    }

    #[test]
    fn project_or_local_scope_detection_matches_official_security_gate() {
        assert!(is_mcp_server_from_project_or_local_settings(&config(
            ConfigScope::Project,
            None
        )));
        assert!(is_mcp_server_from_project_or_local_settings(&config(
            ConfigScope::Local,
            None
        )));
        assert!(!is_mcp_server_from_project_or_local_settings(&config(
            ConfigScope::User,
            None
        )));
        assert!(!is_mcp_server_from_project_or_local_settings(&config(
            ConfigScope::Dynamic,
            None
        )));
    }

    #[tokio::test]
    async fn get_mcp_server_headers_merges_dynamic_over_static_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let helper = "printf '{\"X-Dynamic\":\"dynamic\",\"X-Override\":\"dynamic\"}'";
        let headers =
            get_mcp_server_headers("docs", &config(ConfigScope::User, Some(helper.to_string())))
                .await;
        assert_eq!(headers.get("X-Static").map(String::as_str), Some("static"));
        assert_eq!(
            headers.get("X-Dynamic").map(String::as_str),
            Some("dynamic")
        );
        assert_eq!(
            headers.get("X-Override").map(String::as_str),
            Some("dynamic")
        );
    }

    #[tokio::test]
    async fn get_mcp_headers_from_helper_blocks_untrusted_project_scope_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-mcp-headers-helper-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp_dir.join("config"));
        let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
        let _non_interactive = EnvVarGuard::unset("COMETIX_NON_INTERACTIVE_SESSION");
        crate::bootstrap::state::set_original_cwd(temp_dir.join("workspace"));
        crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();

        let blocked = get_mcp_headers_from_helper(
            "docs",
            &config(
                ConfigScope::Project,
                Some("printf '{\"X-Dynamic\":\"dynamic\"}'".to_string()),
            ),
        )
        .await;
        assert!(blocked.is_none());

        crate::bootstrap::state::set_original_cwd(previous_original_cwd);
        drop((_config, _non_interactive));
        crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
