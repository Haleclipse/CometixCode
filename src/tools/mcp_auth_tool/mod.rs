//! Maps to: CC `tools/McpAuthTool/McpAuthTool.ts`.

/// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:27` `McpAuthOutput.status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpAuthStatus {
    AuthUrl,
    Unsupported,
    Error,
}

/// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:26-30` `McpAuthOutput`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpAuthOutput {
    pub status: McpAuthStatus,
    pub message: String,
    pub auth_url: Option<String>,
}

/// Rust registry projection of the Tool object returned by
/// CC `tools/McpAuthTool/McpAuthTool.ts:49-215` `createMcpAuthTool`.
pub struct McpAuthTool {
    server_name: String,
    config: crate::services::mcp::types::ScopedMcpServerConfig,
    tool: crate::types::tools::Tool,
    user_facing_name: String,
    tool_use_message: String,
}

/// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:32-35` `getConfigUrl`.
fn get_config_url(config: &crate::services::mcp::types::ScopedMcpServerConfig) -> Option<&str> {
    config.url.as_deref()
}

/// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:49-215` `createMcpAuthTool`.
pub fn create_mcp_auth_tool(
    server_name: &str,
    config: &crate::services::mcp::types::ScopedMcpServerConfig,
) -> McpAuthTool {
    let transport = config.transport.as_str();
    let location = get_config_url(config)
        .map(|url| format!("{transport} at {url}"))
        .unwrap_or_else(|| transport.to_string());
    let description = format!(
        "The `{server_name}` MCP server ({location}) is installed but requires authentication. Call this tool to start the OAuth flow — you'll receive an authorization URL to share with the user. Once the user completes authorization in their browser, the server's real tools will become available automatically."
    );

    McpAuthTool {
        server_name: server_name.to_string(),
        config: config.clone(),
        user_facing_name: format!("{server_name} - authenticate (MCP)"),
        tool_use_message: format!("Authenticate {server_name} MCP server"),
        // CC `:62-84`: always the prefixed name; `description()` and
        // `prompt()` both return the description; `isReadOnly()` and
        // `isConcurrencySafe()` are false. No title, so
        // `mcp_user_facing_name()` is `:70`'s string.
        tool: crate::types::tools::Tool {
            name: crate::services::mcp::mcp_string_utils::build_mcp_tool_name(
                server_name,
                "authenticate",
            ),
            description: description.clone(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
            is_mcp: true,
            mcp_info: Some(crate::types::tools::McpToolInfo {
                server_name: server_name.to_string(),
                tool_name: "authenticate".to_string(),
            }),
            mcp_description: description,
            mcp_connection: Some(crate::types::tools::McpToolConnection::Authenticate {
                server_name: server_name.to_string(),
                config: config.clone(),
            }),
            ..Default::default()
        },
    }
}

impl McpAuthTool {
    /// The tool object `createMcpAuthTool` returns, as it joins `mcp.tools`.
    pub fn tool(&self) -> crate::types::tools::Tool {
        self.tool.clone()
    }

    /// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:70`
    /// `createMcpAuthTool(...).userFacingName`.
    pub fn user_facing_name(&self) -> &str {
        &self.user_facing_name
    }

    /// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:72`
    /// `createMcpAuthTool(...).renderToolUseMessage`.
    pub fn render_tool_use_message(&self) -> &str {
        &self.tool_use_message
    }

    /// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:71`
    /// `createMcpAuthTool(...).maxResultSizeChars`.
    pub fn max_result_size_chars(&self) -> usize {
        10_000
    }

    /// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:69`
    /// `createMcpAuthTool(...).toAutoClassifierInput`.
    pub fn to_auto_classifier_input(&self) -> &str {
        &self.server_name
    }

    /// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:85-206`
    /// `createMcpAuthTool(...).call`.
    pub async fn call(&self, context: &crate::tool::ToolUseContext) -> McpAuthOutput {
        use crate::services::mcp::types::Transport;

        if self.config.transport != Transport::Sse && self.config.transport != Transport::Http {
            return McpAuthOutput {
                status: McpAuthStatus::Unsupported,
                message: format!(
                    "Server \"{}\" uses {} transport which does not support OAuth from this tool. Ask the user to run /mcp and authenticate manually.",
                    self.server_name,
                    self.config.transport.as_str()
                ),
                auth_url: None,
            };
        }

        let (sender, receiver) = tokio::sync::oneshot::channel::<Result<Option<String>, String>>();
        let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(sender)));
        let url_sender = sender.clone();
        let completion_sender = sender.clone();
        let background_server_name = self.server_name.clone();
        let background_config = self.config.clone();
        let background_app_store = context.app_store.clone();
        tokio::spawn(async move {
            let on_authorization_url = std::sync::Arc::new(move |url: String| {
                if let Ok(mut sender) = url_sender.lock()
                    && let Some(sender) = sender.take()
                {
                    let _ = sender.send(Ok(Some(url)));
                }
            });
            let result = crate::services::mcp::auth::perform_mcp_oauth_flow(
                &background_server_name,
                &background_config,
                crate::services::mcp::auth::McpOAuthFlowOptions {
                    skip_browser_open: true,
                    on_authorization_url: Some(on_authorization_url),
                    ..Default::default()
                },
            )
            .await;
            match result {
                Ok(_) => {
                    if let Ok(mut sender) = completion_sender.lock()
                        && let Some(sender) = sender.take()
                    {
                        let _ = sender.send(Ok(None));
                    }
                    // Background continuation: once OAuth completes, reconnect
                    // and swap the real tools into appState. Prefix-based
                    // replacement removes this pseudo-tool since it shares the
                    // `mcp__<server>__` prefix.
                    crate::services::mcp::client::clear_mcp_auth_cache();
                    let result = crate::services::mcp::client::reconnect_mcp_server_impl(
                        &background_server_name,
                        &background_config,
                    )
                    .await;
                    let tool_count = result.tools.len();
                    if background_app_store.writable
                        && let Some(store) = background_app_store.store.as_ref()
                    {
                        store.replace_with(|app| {
                            let mut mcp = (*app.mcp).clone();
                            // Maps to: CC McpAuthTool.ts:146-168, whose
                            // updater intentionally differs from the hook:
                            // map existing clients only; use prefix-only
                            // command removal; retain absent resources.
                            let prefix = format!(
                                "mcp__{}__",
                                crate::services::mcp::normalization::normalize_name_for_mcp(
                                    &background_server_name
                                )
                            );
                            if let Some(existing) = mcp
                                .clients
                                .iter_mut()
                                .find(|client| client.name() == background_server_name)
                            {
                                *existing = result.server;
                            }
                            mcp.tools.retain(|tool| !tool.name.starts_with(&prefix));
                            mcp.tools.extend(result.tools);
                            mcp.commands
                                .retain(|command| !command.name.starts_with(&prefix));
                            mcp.commands.extend(result.commands);
                            if let Some(resources) = result.resources {
                                mcp.resources
                                    .insert(background_server_name.clone(), resources);
                            }
                            app.mcp = std::sync::Arc::new(mcp);
                        });
                    }
                    tracing::debug!(
                        server = %background_server_name,
                        "OAuth complete, reconnected with {tool_count} tool(s)"
                    );
                }
                Err(error) => {
                    if let Ok(mut sender) = completion_sender.lock()
                        && let Some(sender) = sender.take()
                    {
                        let _ = sender.send(Err(error.to_string()));
                    }
                    tracing::warn!(server = %background_server_name, error = %error, "MCP auth pseudo-tool flow failed");
                }
            }
        });

        match receiver.await {
            Ok(Ok(Some(auth_url))) => McpAuthOutput {
                status: McpAuthStatus::AuthUrl,
                message: format!(
                    "Ask the user to open this URL in their browser to authorize the {} MCP server:\n\n{}\n\nOnce they complete the flow, the server's tools will become available automatically.",
                    self.server_name, auth_url
                ),
                auth_url: Some(auth_url),
            },
            Ok(Ok(None)) => McpAuthOutput {
                status: McpAuthStatus::AuthUrl,
                message: format!(
                    "Authentication completed silently for {}. The server's tools should now be available.",
                    self.server_name
                ),
                auth_url: None,
            },
            Ok(Err(error)) => McpAuthOutput {
                status: McpAuthStatus::Error,
                message: format!(
                    "Failed to start OAuth flow for {}: {}. Ask the user to run /mcp and authenticate manually.",
                    self.server_name, error
                ),
                auth_url: None,
            },
            // L1-only branch with no CC counterpart: a JS Promise.race always
            // settles with a value, but a dropped oneshot sender (background
            // task panic) closes the channel. The copy mirrors the catch-arm
            // framing so the model sees the same guidance shape.
            Err(_) => McpAuthOutput {
                status: McpAuthStatus::Error,
                message: format!(
                    "Failed to start OAuth flow for {}: authentication task ended before producing an authorization URL. Ask the user to run /mcp and authenticate manually.",
                    self.server_name
                ),
                auth_url: None,
            },
        }
    }

    /// Maps to: CC `tools/McpAuthTool/McpAuthTool.ts:207-213`
    /// `mapToolResultToToolResultBlockParam`.
    ///
    /// Rust's typed transcript result is the established model/UI projection
    /// of the same tool-result ID and message content.
    pub fn map_tool_result_to_tool_result_block_param(
        &self,
        request: &crate::types::permissions::PermissionRequest,
        output: McpAuthOutput,
    ) -> crate::types::message::RenderableMessage {
        // CC records the tool's `McpAuthOutput` as `toolUseResult`
        // (`McpAuthTool.ts:26-30`); `createMcpAuthTool` defines no
        // renderToolResultMessage, so the render layer hides the success row
        // by name+status (`success_tool_result_is_nonvisual`).
        let mut raw = serde_json::Map::new();
        raw.insert(
            "status".to_string(),
            serde_json::json!(match output.status {
                McpAuthStatus::AuthUrl => "auth_url",
                McpAuthStatus::Unsupported => "unsupported",
                McpAuthStatus::Error => "error",
            }),
        );
        raw.insert(
            "message".to_string(),
            serde_json::json!(output.message.clone()),
        );
        if let Some(auth_url) = output.auth_url.as_deref() {
            raw.insert("authUrl".to_string(), serde_json::json!(auth_url));
        }
        crate::types::message::RenderableMessage::user_tool_result(
            uuid::Uuid::new_v4().to_string(),
            request.tool_use_id.clone(),
            output.message,
            false,
        )
        .with_tool_use_result(Some(serde_json::Value::Object(raw)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_mcp_auth_tool_keeps_official_descriptor_copy() {
        let config = crate::services::mcp::types::ScopedMcpServerConfig {
            name: None,
            scope: crate::services::mcp::types::ConfigScope::Local,
            transport: crate::services::mcp::types::Transport::Http,
            command: None,
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            url: Some("https://mcp.example.com".to_string()),
            headers: std::collections::BTreeMap::new(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            plugin_source: None,
        };
        let tool = create_mcp_auth_tool("linear", &config);
        let descriptor = tool.tool();
        assert_eq!(descriptor.name, "mcp__linear__authenticate");
        assert!(descriptor.is_mcp);
        assert!(
            descriptor
                .description
                .contains("http at https://mcp.example.com")
        );
        assert_eq!(descriptor.mcp_description, descriptor.description);
        assert!(!descriptor.mcp_annotations.read_only_hint);
        assert_eq!(tool.user_facing_name(), "linear - authenticate (MCP)");
        assert_eq!(
            descriptor.mcp_user_facing_name().as_deref(),
            Some(tool.user_facing_name())
        );
        assert_eq!(
            tool.render_tool_use_message(),
            "Authenticate linear MCP server"
        );
    }
}
