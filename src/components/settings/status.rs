//! Maps to: CC components/Settings/Status.tsx
//! Status tab — shows session info, model, version, account diagnostics.
//! Nyan keeps this as a main-screen local command UI and only reads
//! already-known runtime/config snapshots.

use crate::constants::figures::figures;
use crate::constants::product;
use crate::services::mcp::types::{McpServerConnection, McpServerConnectionType};
use crate::utils::claudemd::{ClaudeMdFile, discover_claude_md_files};
use crate::utils::file::get_display_path;
use crate::utils::format::format_number;
use crate::utils::ide::IDEExtensionInstallationStatus;
use crate::utils::model::model::render_model_name;
use crate::utils::settings::get_managed_settings_file_path;
use crate::utils::settings::types::CUSTOMIZATION_SURFACES;
use crate::utils::settings::{SettingsJson, ValidationError};
use crate::utils::status::StartupDiagnosticsSnapshot;
use crate::utils::status::{
    Property as StatusPropertyData, build_account_properties, build_api_provider_properties,
};
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct StatusProps {
    pub session_id: Option<String>,
    pub session_name: Option<String>,
}

/// Maps to: CC `components/Settings/Status.tsx:109-169` `Status`.
#[component]
pub fn Status(props: &StatusProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    // Maps to: CC `useAppState(s => s.settings)` + `getGlobalConfig()` cached
    // direct reads; the remaining context carries startup diagnostics only.
    let runtime_config = hooks.try_use_context::<StartupDiagnosticsSnapshot>();
    let settings_snapshot =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.settings.clone());
    let settings = settings_snapshot.as_ref();
    let resolved_model = crate::utils::model::model::get_main_loop_model();
    let model_display_name = render_model_name(&resolved_model);
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| {
            crate::bootstrap::state::get_original_cwd()
                .display()
                .to_string()
        });
    let settings_errors = runtime_config
        .as_ref()
        .map(|context| context.settings_errors.as_slice())
        .unwrap_or(&[]);
    let installation_diagnostics = runtime_config
        .as_ref()
        .map(|context| context.installation_diagnostics.as_slice())
        .unwrap_or(&[]);
    let doctor_diagnostics = runtime_config
        .as_ref()
        .map(|context| context.doctor_diagnostics.as_slice())
        .unwrap_or(&[]);
    let diagnostics = status_diagnostics(
        settings_errors,
        installation_diagnostics,
        doctor_diagnostics,
    );
    let session_id = props
        .session_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(crate::bootstrap::state::get_session_id);
    let session_name = props
        .session_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "/rename to add a name".to_string());
    let account_properties = build_account_properties();
    let api_provider_properties = build_api_provider_properties();
    // CC `useAppState(s => s.mcp)` (`Status.tsx:114`).
    let mcp_clients =
        crate::state::app_state::use_app_state(&mut hooks, |state| state.mcp.clients.clone());
    let ide_properties = ide_status_properties(
        &mcp_clients,
        runtime_config
            .as_ref()
            .and_then(|context| context.ide_installation_status.as_ref()),
    );
    let mcp_properties = mcp_status_properties(&mcp_clients);
    let sandbox_properties = sandbox_status_properties(settings);
    let setting_sources = crate::utils::status::build_setting_sources_properties();
    let setting_sources_text = (!setting_sources.is_empty()).then(|| setting_sources.join(", "));

    element! {
        View(flex_direction: FlexDirection::Column, padding_left: 1u32) {
            StatusProperty(label: "Version", value: product::VERSION.to_string())
            StatusProperty(label: "Session name", value: session_name)
            StatusProperty(label: "Session ID", value: session_id)
            StatusProperty(label: "cwd", value: cwd.clone())
            #(account_properties.into_iter().map(|property| element! {
                StatusProperty(label: property.label, value: property.value)
            }).collect::<Vec<_>>())
            #(api_provider_properties.into_iter().map(|property| element! {
                StatusProperty(label: property.label, value: property.value)
            }).collect::<Vec<_>>())
            StatusProperty(label: "Model", value: model_display_name.clone())
            #(ide_properties.into_iter().map(|property| element! {
                StatusProperty(label: property.label, value: property.value)
            }).collect::<Vec<_>>())
            #(mcp_properties.into_iter().map(|property| element! {
                StatusProperty(label: property.label, value: property.value)
            }).collect::<Vec<_>>())
            #(sandbox_properties.into_iter().map(|property| element! {
                StatusProperty(label: property.label, value: property.value)
            }).collect::<Vec<_>>())
            #(setting_sources_text.map(|sources| element! {
                StatusProperty(label: "Setting sources", value: sources)
            }))
            #(if diagnostics.is_empty() {
                None
            } else {
                Some(element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "System Diagnostics", weight: Weight::Bold)
                        #(diagnostics.into_iter().map(|diagnostic| element! {
                            View(flex_direction: FlexDirection::Row, padding_left: 1u32) {
                                Text(content: figures().warning, color: theme.error, wrap: TextWrap::NoWrap)
                                Text(content: " ")
                                Text(content: diagnostic)
                            }
                        }).collect::<Vec<_>>())
                    }
                })
            })
        }
    }
}

/// Maps to: CC `utils/status.tsx:69-146` `buildIDEProperties`.
fn ide_status_properties(
    mcp_clients: &[McpServerConnection],
    ide_installation_status: Option<&IDEExtensionInstallationStatus>,
) -> Vec<StatusPropertyData> {
    let ide_client = mcp_clients.iter().find(|client| client.name() == "ide");

    if let Some(status) = ide_installation_status {
        let ide_name = ide_display_name(status.ide_type.as_deref());
        let plugin_or_extension = if is_jetbrains_ide(status.ide_type.as_deref()) {
            "plugin"
        } else {
            "extension"
        };

        if let Some(error) = status
            .error
            .as_deref()
            .map(str::trim)
            .filter(|error| !error.is_empty())
        {
            return vec![status_property(
                "IDE",
                format!(
                    "{} Error installing {ide_name} {plugin_or_extension}: {error}\nPlease restart your IDE and try again.",
                    figures().cross
                ),
            )];
        }

        if status.installed {
            let installed_version = status
                .installed_version
                .as_deref()
                .map(str::trim)
                .filter(|version| !version.is_empty())
                .unwrap_or("unknown");
            if let Some(client) = ide_client.and_then(McpServerConnection::as_connected) {
                let server_version = client
                    .server_info
                    .as_ref()
                    .map(|info| info.version.as_str());
                if server_version
                    .map(str::trim)
                    .filter(|version| !version.is_empty())
                    .is_some_and(|server_version| server_version != installed_version)
                {
                    return vec![status_property(
                        "IDE",
                        format!(
                            "Connected to {ide_name} {plugin_or_extension} version {installed_version} (server version: {})",
                            server_version.unwrap_or_default()
                        ),
                    )];
                }
                return vec![status_property(
                    "IDE",
                    format!(
                        "Connected to {ide_name} {plugin_or_extension} version {installed_version}"
                    ),
                )];
            }
            return vec![status_property(
                "IDE",
                format!("Installed {ide_name} {plugin_or_extension}"),
            )];
        }
    } else if let Some(client) = ide_client {
        let ide_name = crate::utils::ide::get_ide_client_name(Some(client))
            .unwrap_or_else(|| "IDE".to_string());
        if client.connection_type() == McpServerConnectionType::Connected {
            return vec![status_property(
                "IDE",
                format!("Connected to {ide_name} extension"),
            )];
        }
        return vec![status_property(
            "IDE",
            format!("{} Not connected to {ide_name}", figures().cross),
        )];
    }

    Vec::new()
}

fn sandbox_status_properties(settings: &SettingsJson) -> Vec<StatusPropertyData> {
    sandbox_status_properties_for_audience(settings, crate::utils::build_profile::build_audience())
}

fn sandbox_status_properties_for_audience(
    settings: &SettingsJson,
    audience: crate::utils::build_profile::BuildAudience,
) -> Vec<StatusPropertyData> {
    if !crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Ui,
    ) {
        return Vec::new();
    }

    let enabled = settings
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.get("enabled"))
        .and_then(|enabled| enabled.as_bool())
        .unwrap_or(false);

    vec![status_property(
        "Bash Sandbox",
        if enabled { "Enabled" } else { "Disabled" },
    )]
}

/// Maps to: CC `utils/status.tsx:148-182` `buildMcpProperties`.
fn mcp_status_properties(mcp_clients: &[McpServerConnection]) -> Vec<StatusPropertyData> {
    let mut connected = 0usize;
    let mut pending = 0usize;
    let mut needs_auth = 0usize;
    let mut failed = 0usize;

    for client in mcp_clients.iter().filter(|client| client.name() != "ide") {
        match client.connection_type() {
            McpServerConnectionType::Connected => connected += 1,
            McpServerConnectionType::Pending => pending += 1,
            McpServerConnectionType::NeedsAuth => needs_auth += 1,
            McpServerConnectionType::Failed | McpServerConnectionType::Disabled => failed += 1,
        }
    }

    let mut parts = Vec::new();
    if connected > 0 {
        parts.push(format!("{connected} connected"));
    }
    if needs_auth > 0 {
        parts.push(format!("{needs_auth} need auth"));
    }
    if pending > 0 {
        parts.push(format!("{pending} pending"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }

    if parts.is_empty() {
        Vec::new()
    } else {
        vec![status_property(
            "MCP servers",
            format!("{} · /mcp", parts.join(", ")),
        )]
    }
}

fn ide_display_name(ide_type: Option<&str>) -> String {
    let Some(ide_type) = ide_type.map(str::trim).filter(|value| !value.is_empty()) else {
        return "IDE".to_string();
    };

    match ide_type.to_ascii_lowercase().as_str() {
        "code" | "vscode" | "visual-studio-code" => "VS Code".to_string(),
        "cursor" => "Cursor".to_string(),
        "windsurf" => "Windsurf".to_string(),
        "antigravity" => "Antigravity".to_string(),
        "intellij" | "intellijidea" | "idea" => "IntelliJ IDEA".to_string(),
        "pycharm" => "PyCharm".to_string(),
        "webstorm" => "WebStorm".to_string(),
        "phpstorm" => "PhpStorm".to_string(),
        "rubymine" => "RubyMine".to_string(),
        "goland" => "GoLand".to_string(),
        "clion" => "CLion".to_string(),
        "rider" => "Rider".to_string(),
        "androidstudio" | "android-studio" => "Android Studio".to_string(),
        other => other
            .split(['-', '_', '.'])
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn is_jetbrains_ide(ide_type: Option<&str>) -> bool {
    ide_type
        .map(|ide_type| {
            matches!(
                ide_type.trim().to_ascii_lowercase().as_str(),
                "intellij"
                    | "intellijidea"
                    | "idea"
                    | "pycharm"
                    | "webstorm"
                    | "phpstorm"
                    | "rubymine"
                    | "goland"
                    | "clion"
                    | "rider"
                    | "androidstudio"
                    | "android-studio"
            )
        })
        .unwrap_or(false)
}

const MAX_MEMORY_CHARACTER_COUNT: usize = 40_000;

fn status_diagnostics(
    settings_errors: &[ValidationError],
    installation_diagnostics: &[String],
    doctor_diagnostics: &[String],
) -> Vec<String> {
    let mut diagnostics = non_empty_diagnostics(installation_diagnostics);
    diagnostics.extend(settings_error_diagnostics(settings_errors));
    diagnostics.extend(non_empty_diagnostics(doctor_diagnostics));
    diagnostics.extend(managed_settings_customization_diagnostics());
    diagnostics.extend(memory_diagnostics_from_files(&discover_claude_md_files()));
    diagnostics
}

fn non_empty_diagnostics(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn managed_settings_customization_diagnostics() -> Vec<String> {
    managed_settings_customization_diagnostics_from_path(&get_managed_settings_file_path())
}

fn managed_settings_customization_diagnostics_from_path(path: &std::path::Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(field) = parsed
        .as_object()
        .and_then(|object| object.get("strictPluginOnlyCustomization"))
    else {
        return Vec::new();
    };

    if field.is_boolean() {
        return Vec::new();
    }

    if let Some(values) = field.as_array() {
        let unknown = values
            .iter()
            .filter_map(|value| value.as_str())
            .filter(|surface| !CUSTOMIZATION_SURFACES.contains(surface))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if unknown.is_empty() {
            return Vec::new();
        }
        return vec![format!(
            "managed-settings.json: strictPluginOnlyCustomization has {} value(s) this client doesn't recognize: {}",
            unknown.len(),
            unknown.join(", ")
        )];
    }

    vec![format!(
        "managed-settings.json: strictPluginOnlyCustomization has an invalid value (expected true or an array, got {})",
        official_json_typeof(field)
    )]
}

fn official_json_typeof(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            "object"
        }
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
    }
}

fn memory_diagnostics_from_files(files: &[ClaudeMdFile]) -> Vec<String> {
    files
        .iter()
        .filter_map(|file| {
            let char_count = file.content.chars().count();
            (char_count > MAX_MEMORY_CHARACTER_COUNT).then(|| {
                format!(
                    "Large {} will impact performance ({} chars > {} chars)",
                    get_display_path(&file.path.display().to_string()),
                    format_number(char_count as u64),
                    format_number(MAX_MEMORY_CHARACTER_COUNT as u64),
                )
            })
        })
        .collect()
}

fn settings_error_diagnostics(errors: &[ValidationError]) -> Vec<String> {
    if errors.is_empty() {
        return Vec::new();
    }

    let mut files = errors
        .iter()
        .map(|error| {
            error
                .file
                .as_deref()
                .map(str::trim)
                .filter(|file| !file.is_empty())
                .unwrap_or("settings")
                .to_string()
        })
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();

    vec![format!(
        "Found invalid settings files: {}. They will be ignored.",
        files.join(", ")
    )]
}

fn status_property(label: &'static str, value: impl Into<String>) -> StatusPropertyData {
    StatusPropertyData {
        label: Some(label),
        value: value.into(),
    }
}

#[derive(Default, Props)]
struct StatusPropertyProps {
    label: Option<&'static str>,
    value: String,
}

#[component]
fn StatusProperty(props: &StatusPropertyProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    element! {
        View(flex_direction: FlexDirection::Row) {
            #(props.label.map(|label| element! {
                View(width: 20u32) {
                    Text(content: label, color: theme.inactive)
                }
            }))
            Text(content: props.value.clone(), color: theme.text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mcp::types::{ConfigScope, ScopedMcpServerConfig, Transport};
    use crate::utils::config::GlobalConfig;
    use crate::utils::ide::IDEExtensionInstallationStatus;
    use crate::utils::theme;
    use std::sync::Arc;

    /// The `ide` client's config as the IDE integration builds it: an
    /// `sse-ide` transport carrying the IDE's `ideName`, which is what CC
    /// `getIdeClientName` (`utils/ide.ts:1188-1196`) reads.
    fn ide_config(ide_name: &str) -> ScopedMcpServerConfig {
        ScopedMcpServerConfig {
            scope: ConfigScope::Dynamic,
            transport: Transport::SseIde,
            command: None,
            url: Some("http://127.0.0.1:1234/sse".to_string()),
            ide_name: Some(ide_name.to_string()),
            ..ScopedMcpServerConfig::stdio_for_test("unused")
        }
    }

    /// A connected `ide` client whose `serverInfo.version` is `server_version`.
    fn connected_ide(ide_name: &str, server_version: &str) -> McpServerConnection {
        let mut client = McpServerConnection::connected_for_test("ide", ide_config(ide_name));
        if let McpServerConnection::Connected(connected) = &mut client {
            connected.server_info = Some(rmcp::model::Implementation::new("ide", server_version));
        }
        client
    }

    fn stdio_client(name: &str, connection_type: McpServerConnectionType) -> McpServerConnection {
        let config = ScopedMcpServerConfig::stdio_for_test(&format!("{name}-mcp"));
        match connection_type {
            McpServerConnectionType::Connected => {
                McpServerConnection::connected_for_test(name, config)
            }
            McpServerConnectionType::Failed => McpServerConnection::failed(name, config, None),
            McpServerConnectionType::NeedsAuth => McpServerConnection::needs_auth(name, config),
            McpServerConnectionType::Pending => McpServerConnection::pending(name, config),
            McpServerConnectionType::Disabled => McpServerConnection::disabled(name, config),
        }
    }

    #[test]
    fn ide_status_properties_match_official_installation_and_client_rows_readonly() {
        let connected_ide = connected_ide("Cursor", "1.2.0");
        let status = IDEExtensionInstallationStatus {
            installed: true,
            error: None,
            installed_version: Some("1.3.0".to_string()),
            ide_type: Some("cursor".to_string()),
        };

        assert_eq!(
            ide_status_properties(std::slice::from_ref(&connected_ide), Some(&status)),
            vec![status_property(
                "IDE",
                "Connected to Cursor extension version 1.3.0 (server version: 1.2.0)"
            )]
        );

        let matching_status = IDEExtensionInstallationStatus {
            installed_version: Some("1.2.0".to_string()),
            ..status.clone()
        };
        assert_eq!(
            ide_status_properties(std::slice::from_ref(&connected_ide), Some(&matching_status)),
            vec![status_property(
                "IDE",
                "Connected to Cursor extension version 1.2.0"
            )]
        );

        let error_status = IDEExtensionInstallationStatus {
            installed: false,
            error: Some("permission denied".to_string()),
            installed_version: None,
            ide_type: Some("pycharm".to_string()),
        };
        assert_eq!(
            ide_status_properties(&[], Some(&error_status)),
            vec![status_property(
                "IDE",
                format!(
                    "{} Error installing PyCharm plugin: permission denied\nPlease restart your IDE and try again.",
                    figures().cross
                )
            )]
        );

        let disconnected_ide = McpServerConnection::failed("ide", ide_config("VS Code"), None);
        assert_eq!(
            ide_status_properties(&[disconnected_ide], None),
            vec![status_property(
                "IDE",
                format!("{} Not connected to VS Code", figures().cross)
            )]
        );

        assert_eq!(
            ide_status_properties(std::slice::from_ref(&connected_ide), None),
            vec![status_property("IDE", "Connected to Cursor extension")]
        );
    }

    #[test]
    fn mcp_status_properties_match_official_summary_counts_readonly() {
        let rows = mcp_status_properties(&[
            connected_ide("Cursor", "1.0"),
            stdio_client("memory", McpServerConnectionType::Connected),
            stdio_client("docs", McpServerConnectionType::NeedsAuth),
            stdio_client("repo", McpServerConnectionType::Pending),
            stdio_client("slack", McpServerConnectionType::Failed),
            stdio_client("old", McpServerConnectionType::Disabled),
        ]);

        assert_eq!(
            rows,
            vec![status_property(
                "MCP servers",
                "1 connected, 1 need auth, 1 pending, 2 failed · /mcp"
            )]
        );
        assert!(mcp_status_properties(&[]).is_empty());
    }

    #[test]
    fn sandbox_status_properties_match_official_internal_gate_readonly() {
        use crate::utils::build_profile::BuildAudience;

        let mut settings = SettingsJson::default();
        settings.sandbox = Some(serde_json::json!({ "enabled": true }));

        assert_eq!(
            sandbox_status_properties_for_audience(&settings, BuildAudience::AnthropicInternal,),
            vec![status_property("Bash Sandbox", "Enabled")]
        );

        settings.sandbox = Some(serde_json::json!({ "enabled": false }));
        assert_eq!(
            sandbox_status_properties_for_audience(&settings, BuildAudience::AnthropicInternal,),
            vec![status_property("Bash Sandbox", "Disabled")]
        );
        assert!(
            sandbox_status_properties_for_audience(&settings, BuildAudience::External,).is_empty()
        );
    }

    #[test]
    fn memory_diagnostics_match_official_large_file_message_readonly() {
        let diagnostics = memory_diagnostics_from_files(&[
            ClaudeMdFile {
                path: std::path::PathBuf::from("/tmp/AGENTS.md"),
                source: crate::utils::claudemd::ClaudeMdSource::Project,
                kind: crate::utils::claudemd::ClaudeMdKind::Project,
                content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT),
                parent: None,
                is_nested: false,
            },
            ClaudeMdFile {
                path: std::path::PathBuf::from("/tmp/large-AGENTS.md"),
                source: crate::utils::claudemd::ClaudeMdSource::Project,
                kind: crate::utils::claudemd::ClaudeMdKind::Project,
                content: "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 1),
                parent: None,
                is_nested: false,
            },
        ]);

        assert_eq!(
            diagnostics,
            vec![
                "Large /tmp/large-AGENTS.md will impact performance (40.0k chars > 40.0k chars)"
                    .to_string()
            ]
        );
    }

    #[test]
    fn settings_error_diagnostics_match_official_invalid_files_message() {
        let diagnostics = settings_error_diagnostics(&[
            ValidationError {
                file: Some("/tmp/project/.nyan/settings.json".to_string()),
                path: "model".to_string(),
                message: "Invalid model".to_string(),
                expected: None,
                invalid_value: None,
                doc_link: None,
                suggestion: None,
            },
            ValidationError {
                file: Some("/tmp/project/.nyan/settings.json".to_string()),
                path: "permissions".to_string(),
                message: "Invalid permission".to_string(),
                expected: None,
                invalid_value: None,
                doc_link: None,
                suggestion: None,
            },
        ]);

        assert_eq!(
            diagnostics,
            vec![
                "Found invalid settings files: /tmp/project/.nyan/settings.json. They will be ignored."
                    .to_string(),
            ]
        );
    }

    #[test]
    fn status_diagnostics_orders_install_health_and_settings_like_official_readonly() {
        let settings_errors = vec![ValidationError {
            file: Some("/tmp/project/.nyan/settings.json".to_string()),
            path: "model".to_string(),
            message: "Invalid model".to_string(),
            expected: None,
            invalid_value: None,
            doc_link: None,
            suggestion: None,
        }];
        let installation = vec!["Add ~/.local/bin to PATH".to_string(), " ".to_string()];
        let doctor =
            vec!["Native installation exists but ~/.local/bin is not in your PATH".to_string()];

        let diagnostics = status_diagnostics(&settings_errors, &installation, &doctor);

        assert_eq!(diagnostics[0], "Add ~/.local/bin to PATH");
        assert_eq!(
            diagnostics[1],
            "Found invalid settings files: /tmp/project/.nyan/settings.json. They will be ignored."
        );
        assert_eq!(
            diagnostics[2],
            "Native installation exists but ~/.local/bin is not in your PATH"
        );
    }

    #[test]
    fn doctor_diagnostics_match_official_managed_settings_customization_warnings() {
        let root = std::env::temp_dir().join(format!(
            "nyan-status-managed-diagnostics-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("create temp root");
        let path = root.join("managed-settings.json");

        std::fs::write(
            &path,
            r#"{"strictPluginOnlyCustomization":{"skills":true}}"#,
        )
        .expect("write invalid managed settings");
        assert_eq!(
            managed_settings_customization_diagnostics_from_path(&path),
            vec![
                "managed-settings.json: strictPluginOnlyCustomization has an invalid value (expected true or an array, got object)"
                    .to_string()
            ]
        );

        std::fs::write(
            &path,
            r#"{"strictPluginOnlyCustomization":["skills","widgets",7,"agents","portals"]}"#,
        )
        .expect("write unknown managed settings surfaces");
        assert_eq!(
            managed_settings_customization_diagnostics_from_path(&path),
            vec![
                "managed-settings.json: strictPluginOnlyCustomization has 2 value(s) this client doesn't recognize: widgets, portals"
                    .to_string()
            ]
        );

        std::fs::write(&path, r#"{"strictPluginOnlyCustomization":true}"#)
            .expect("write boolean managed settings");
        assert!(managed_settings_customization_diagnostics_from_path(&path).is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn status_reads_readonly_runtime_model_and_session_metadata() {
        let _env_guard = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::config::set_test_global_config(Some(GlobalConfig::default()));
        let settings = SettingsJson::default();
        crate::utils::settings::settings_cache::install_test_settings(
            serde_json::from_value(serde_json::json!({"nyan": {
                "providers": {"p": {"kind": "anthropic", "protocol": "anthropicMessages"}},
                "models": {"main": {"provider": "p", "model": "claude-opus-4-6",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}}},
                "selection": {"main": "main"}
            }}))
            .unwrap(),
        );
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.settings = Arc::new(settings);
        let test_store = crate::state::store::AppStore::new(initial, None);

        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(test_store),
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        Status(
                            session_id: Some("session-123".to_string()),
                            session_name: Some("Investigation".to_string()),
                        )
                    }.into_any()),
                )
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("Session name"), "canvas=\n{text}");
        assert!(text.contains("Investigation"), "canvas=\n{text}");
        assert!(text.contains("Session ID"), "canvas=\n{text}");
        assert!(text.contains("session-123"), "canvas=\n{text}");
        assert!(text.contains("Model"), "canvas=\n{text}");
        assert!(text.contains("Opus"), "canvas=\n{text}");
        crate::utils::settings::settings_cache::reset_settings_cache();
    }

    #[test]
    fn status_renders_readonly_install_and_doctor_diagnostics_from_runtime_snapshot() {
        let runtime_config = StartupDiagnosticsSnapshot {
            settings_errors: Vec::new(),
            installation_diagnostics: vec!["Install setup warning".to_string()],
            doctor_diagnostics: vec!["Doctor health warning".to_string()],
            ide_installation_status: None,
        };

        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ContextProvider(value: Context::owned(runtime_config)) {
                    // Status reads `state.settings` for the model row and
                    // `state.mcp` for the IDE/MCP rows. Defaults are the
                    // fixture: this test asserts on the diagnostics, which
                    // come from the StartupDiagnosticsSnapshot above.
                    crate::state::app_state::AppStateProvider(
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            Status(
                                session_id: Some("session-123".to_string()),
                                session_name: Some("Investigation".to_string()),
                            )
                        }.into_any()),
                    )
                }
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("System Diagnostics"), "canvas=\n{text}");
        assert!(text.contains("Install setup warning"), "canvas=\n{text}");
        assert!(text.contains("Doctor health warning"), "canvas=\n{text}");
    }

    #[test]
    fn status_renders_readonly_ide_and_mcp_summary_from_app_state_clients() {
        let runtime_config = StartupDiagnosticsSnapshot {
            settings_errors: Vec::new(),
            installation_diagnostics: Vec::new(),
            doctor_diagnostics: Vec::new(),
            ide_installation_status: Some(IDEExtensionInstallationStatus {
                installed: true,
                error: None,
                installed_version: Some("1.3.0".to_string()),
                ide_type: Some("cursor".to_string()),
            }),
        };
        // CC `Status.tsx:114` reads `useAppState(s => s.mcp)` and hands
        // `mcp.clients` to both `buildIDEProperties` and `buildMcpProperties`.
        let initial_state = crate::state::app_state_store::AppState {
            mcp: Arc::new(crate::state::app_state_store::McpState {
                clients: vec![
                    connected_ide("Cursor", "1.2.0"),
                    stdio_client("memory", McpServerConnectionType::Connected),
                    stdio_client("repo", McpServerConnectionType::Failed),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };

        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ContextProvider(value: Context::owned(runtime_config)) {
                    crate::state::app_state::AppStateProvider(
                        initial_state: Some(initial_state),
                        children: crate::state::app_state::ProviderChildren::new(|| element! {
                            Status(
                                session_id: Some("session-123".to_string()),
                                session_name: Some("Investigation".to_string()),
                            )
                        }.into_any()),
                    )
                }
            }
        }
        .render(None)
        .to_string();

        assert!(text.contains("IDE"), "canvas=\n{text}");
        assert!(
            text.contains("Connected to Cursor extension version 1.3.0 (server version: 1.2.0)"),
            "canvas=\n{text}"
        );
        assert!(text.contains("MCP servers"), "canvas=\n{text}");
        assert!(
            text.contains("1 connected, 1 failed · /mcp"),
            "canvas=\n{text}"
        );
    }
}
