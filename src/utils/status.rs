//! Maps to: CC `utils/status.tsx`.

use crate::services::mcp::types::{McpClientSnapshot, McpServerConnectionType};
use crate::utils::config::{GlobalConfig, ProjectConfig};
use crate::utils::process_env::JsTruthy;
use crate::utils::settings::constants::get_setting_source_display_name_capitalized;
use crate::utils::settings::{
    SettingSource, SettingsJson, get_enabled_setting_sources, get_managed_file_settings_presence,
    get_settings_for_source,
};
use std::collections::{BTreeMap, BTreeSet};

/// Startup-only diagnostics snapshot for Settings/Status surfaces. Read-only,
/// built once at App mount and injected directly (no wrapper context class).
/// settings/globalConfig were absorbed: settings live in `AppState.settings`
/// (CC AppStateStore.ts:90) and globalConfig reads go through the cached
/// `load_global_config()` (CC `getGlobalConfig()`).
///
/// Rust-only aggregate, no CC counterpart type: upstream fetches each of these
/// diagnostics separately at the Status surfaces. It lives with the rest of the
/// Status data layer rather than in `state/`, whose CC original declares
/// nothing of the kind.
#[derive(Clone, Debug, Default)]
pub struct StartupDiagnosticsSnapshot {
    /// Readonly startup settings validation diagnostics for Settings/Status.
    /// These are display-only; no config/session files are rewritten.
    pub settings_errors: Vec<crate::utils::settings::ValidationError>,
    /// Already-known native installer warnings for Settings/Status diagnostics.
    /// Cometix does not run installer probes from the main-screen UI.
    pub installation_diagnostics: Vec<String>,
    /// Already-known doctor warnings for Settings/Status diagnostics.
    /// Cometix does not run external doctor probes from the main-screen UI.
    pub doctor_diagnostics: Vec<String>,
    /// Already-known MCP client snapshots for Settings/Status summaries.
    /// Cometix never starts MCP clients from Status.
    pub mcp_clients: Vec<McpClientSnapshot>,
    /// Already-known IDE installation status for Settings/Status summaries.
    /// Cometix never installs IDE extensions from Status.
    pub ide_installation_status: Option<crate::utils::ide::IDEExtensionInstallationStatus>,
}

fn project_mcp_disabled_names(project_config: &ProjectConfig) -> BTreeSet<String> {
    project_config
        .disabled_mcp_servers
        .iter()
        .flatten()
        .chain(project_config.disabled_mcpjson_servers.iter().flatten())
        .filter_map(|name| {
            let trimmed = name.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

fn project_mcp_enabled_names(project_config: &ProjectConfig) -> BTreeSet<String> {
    project_config
        .enabled_mcp_servers
        .iter()
        .flatten()
        .chain(project_config.enabled_mcpjson_servers.iter().flatten())
        .filter_map(|name| {
            let trimmed = name.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

fn insert_mcp_server_names(
    clients: &mut BTreeMap<String, McpServerConnectionType>,
    servers: Option<&serde_json::Value>,
) {
    if let Some(servers) = servers.and_then(serde_json::Value::as_object) {
        for name in servers.keys() {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                continue;
            }
            clients
                .entry(trimmed.to_string())
                .or_insert(McpServerConnectionType::Pending);
        }
    }
}

/// Builds read-only MCP client summaries from global and current project config.
/// This mirrors Status' need for already-known MCP client snapshots without
/// starting MCP clients, authenticating, opening sockets, or writing settings.
///
/// Rust-only, no CC counterpart: upstream reads live connections off
/// `AppState.mcp.clients`, which Status can do because MCP is already running
/// by then. This fills `StartupDiagnosticsSnapshot.mcp_clients` above, which is
/// why it lives here rather than in `state/`.
pub fn mcp_client_snapshots_from_config(
    global_config: &GlobalConfig,
    project_config: &ProjectConfig,
) -> Vec<McpClientSnapshot> {
    let disabled = project_mcp_disabled_names(project_config);
    let enabled = project_mcp_enabled_names(project_config);
    let mut clients = BTreeMap::<String, McpServerConnectionType>::new();

    insert_mcp_server_names(&mut clients, global_config.mcp_servers.as_ref());
    insert_mcp_server_names(&mut clients, project_config.mcp_servers.as_ref());

    for name in enabled {
        clients
            .entry(name)
            .or_insert(McpServerConnectionType::Pending);
    }
    for name in disabled {
        clients.insert(name, McpServerConnectionType::Disabled);
    }

    clients
        .into_iter()
        .map(|(name, status)| McpClientSnapshot {
            ide_name: (name == "ide").then(|| "IDE".to_string()),
            name,
            status,
            reconnect_attempt: None,
            max_reconnect_attempts: None,
            server_version: None,
            error: None,
        })
        .collect()
}

pub fn mcp_client_snapshots_from_project_config(
    project_config: &ProjectConfig,
) -> Vec<McpClientSnapshot> {
    mcp_client_snapshots_from_config(&GlobalConfig::default(), project_config)
}

/// Returns the value projection of CC's `Property[]`; the owning iocraft
/// `Status` component supplies the "Setting sources" label.
///
/// Maps to: CC `utils/status.tsx#buildSettingSourcesProperties`.
pub fn build_setting_sources_properties() -> Vec<String> {
    get_enabled_setting_sources()
        .into_iter()
        .filter_map(|source| {
            let settings = get_settings_for_source(source)?;
            if settings == SettingsJson::default() {
                return None;
            }
            if source == SettingSource::Policy {
                let presence = get_managed_file_settings_presence();
                return Some(
                    if presence.has_base && presence.has_drop_ins {
                        "Enterprise managed settings (file + drop-ins)"
                    } else if presence.has_drop_ins {
                        "Enterprise managed settings (drop-ins)"
                    } else {
                        "Enterprise managed settings (file)"
                    }
                    .to_string(),
                );
            }
            Some(get_setting_source_display_name_capitalized(source).to_string())
        })
        .collect()
}

/// Rust text projection of CC `utils/status.tsx:36-39` `Property`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub label: Option<&'static str>,
    pub value: String,
}

fn property(label: &'static str, value: impl Into<String>) -> Property {
    Property {
        label: Some(label),
        value: value.into(),
    }
}

fn unlabeled_property(value: impl Into<String>) -> Property {
    Property {
        label: None,
        value: value.into(),
    }
}

/// Maps to: CC `utils/status.tsx:286-330` `buildAccountProperties`.
pub fn build_account_properties() -> Vec<Property> {
    let Some(account_info) = crate::utils::auth::get_account_information() else {
        return Vec::new();
    };
    let mut properties = Vec::new();
    if let Some(subscription) = account_info.subscription {
        properties.push(property("Login method", format!("{subscription} Account")));
    }
    if let Some(token_source) = account_info.token_source {
        properties.push(property("Auth token", token_source));
    }
    if let Some(api_key_source) = account_info
        .api_key_source
        .and_then(|source| source.status_label())
    {
        properties.push(property("API key", api_key_source));
    }
    if crate::utils::process_env::var_os("IS_DEMO").as_deref().truthy().is_none() {
        if let Some(organization) = account_info.organization {
            properties.push(property("Organization", organization));
        }
        if let Some(email) = account_info.email {
            properties.push(property("Email", email));
        }
    }
    properties
}

fn push_env_property(properties: &mut Vec<Property>, label: &'static str, key: &str) {
    if let Some(value) = crate::utils::process_env::var(key).truthy() {
        properties.push(property(label, value));
    }
}

/// Maps to: CC `utils/status.tsx:332-460` `buildAPIProviderProperties`.
pub fn build_api_provider_properties() -> Vec<Property> {
    use crate::utils::model::providers::ApiProvider;

    let provider = crate::utils::model::providers::get_api_provider();
    let mut properties = Vec::new();
    match provider {
        ApiProvider::FirstParty => {
            push_env_property(&mut properties, "Anthropic base URL", "ANTHROPIC_BASE_URL");
        }
        ApiProvider::Bedrock => {
            properties.push(property("API provider", "AWS Bedrock"));
            push_env_property(&mut properties, "Bedrock base URL", "BEDROCK_BASE_URL");
            properties.push(property(
                "AWS region",
                crate::utils::env_utils::get_aws_region(),
            ));
            if crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_SKIP_BEDROCK_AUTH")
                    .as_deref(),
            ) {
                properties.push(unlabeled_property("AWS auth skipped"));
            }
        }
        ApiProvider::Vertex => {
            properties.push(property("API provider", "Google Vertex AI"));
            push_env_property(&mut properties, "Vertex base URL", "VERTEX_BASE_URL");
            push_env_property(
                &mut properties,
                "GCP project",
                "ANTHROPIC_VERTEX_PROJECT_ID",
            );
            properties.push(property(
                "Default region",
                crate::utils::env_utils::get_default_vertex_region(),
            ));
            if crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_SKIP_VERTEX_AUTH")
                    .as_deref(),
            ) {
                properties.push(unlabeled_property("GCP auth skipped"));
            }
        }
        ApiProvider::Foundry => {
            properties.push(property("API provider", "Microsoft Foundry"));
            push_env_property(
                &mut properties,
                "Microsoft Foundry base URL",
                "ANTHROPIC_FOUNDRY_BASE_URL",
            );
            push_env_property(
                &mut properties,
                "Microsoft Foundry resource",
                "ANTHROPIC_FOUNDRY_RESOURCE",
            );
            if crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_SKIP_FOUNDRY_AUTH")
                    .as_deref(),
            ) {
                properties.push(unlabeled_property("Microsoft Foundry auth skipped"));
            }
        }
    }

    // Maps to CC `status.tsx:428-457`.
    let env = crate::utils::process_env::snapshot();
    if let Some(proxy_url) = crate::utils::proxy::get_proxy_url(&env) {
        properties.push(property("Proxy", proxy_url));
    }

    let mtls_config = crate::utils::mtls::get_mtls_config();
    let truthy = |key: &str| env.var(key).truthy().map(str::to_owned);
    if let Some(extra_certs) = truthy("NODE_EXTRA_CA_CERTS") {
        properties.push(property("Additional CA cert(s)", extra_certs));
    }
    if let Some(mtls_config) = mtls_config {
        // `mtlsConfig.cert && ...`: an empty file's contents are falsy.
        let loaded = |contents: &Option<String>| contents.as_deref().is_some_and(|c| !c.is_empty());
        if let Some(cert_path) = truthy("CLAUDE_CODE_CLIENT_CERT").filter(|_| loaded(&mtls_config.cert)) {
            properties.push(property("mTLS client cert", cert_path));
        }
        if let Some(key_path) = truthy("CLAUDE_CODE_CLIENT_KEY").filter(|_| loaded(&mtls_config.key)) {
            properties.push(property("mTLS client key", key_path));
        }
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn mcp_client_snapshots_from_project_config_builds_readonly_pending_snapshots() {
        let mut project_config = ProjectConfig::default();
        project_config.mcp_servers = Some(serde_json::json!({"alpha":{},"ide":{},"zeta":{}}));
        project_config.enabled_mcp_servers = Some(vec!["beta".to_string()]);
        project_config.disabled_mcp_servers = Some(vec!["zeta".to_string(), "gamma".to_string()]);

        let clients = mcp_client_snapshots_from_project_config(&project_config);

        assert_eq!(
            clients
                .iter()
                .map(|client| (
                    client.name.as_str(),
                    client.status,
                    client.ide_name.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("alpha", McpServerConnectionType::Pending, None),
                ("beta", McpServerConnectionType::Pending, None),
                ("gamma", McpServerConnectionType::Disabled, None),
                ("ide", McpServerConnectionType::Pending, Some("IDE")),
                ("zeta", McpServerConnectionType::Disabled, None),
            ]
        );
    }

    #[test]
    fn mcp_client_snapshots_from_config_reads_global_and_project_mcp_without_starting_clients() {
        let mut global_config = GlobalConfig::default();
        global_config.mcp_servers = Some(serde_json::json!({"global":{},"shared":{}}));
        let mut project_config = ProjectConfig::default();
        project_config.mcp_servers = Some(serde_json::json!({"project":{},"shared":{}}));
        project_config.disabled_mcp_servers = Some(vec!["global".to_string()]);

        let clients = mcp_client_snapshots_from_config(&global_config, &project_config);

        assert_eq!(
            clients
                .iter()
                .map(|client| (client.name.as_str(), client.status))
                .collect::<Vec<_>>(),
            vec![
                ("global", McpServerConnectionType::Disabled),
                ("project", McpServerConnectionType::Pending),
                ("shared", McpServerConnectionType::Pending),
            ]
        );
    }

    #[test]
    fn build_api_provider_properties_matches_official_process_rows() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cert = std::env::temp_dir().join(format!(
            "cometix-status-cert-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&cert, "certificate").unwrap();
        let guards = vec![
            EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX"),
            EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY"),
            EnvVarGuard::set("BEDROCK_BASE_URL", "https://bedrock.example"),
            EnvVarGuard::set("AWS_DEFAULT_REGION", "us-west-2"),
            EnvVarGuard::set("CLAUDE_CODE_SKIP_BEDROCK_AUTH", "true"),
            EnvVarGuard::set("https_proxy", "http://lowercase-proxy"),
            EnvVarGuard::unset("HTTPS_PROXY"),
            EnvVarGuard::unset("http_proxy"),
            EnvVarGuard::unset("HTTP_PROXY"),
            EnvVarGuard::set("NODE_EXTRA_CA_CERTS", "/tmp/ca.pem"),
            EnvVarGuard::set("CLAUDE_CODE_CLIENT_CERT", &cert),
            EnvVarGuard::unset("CLAUDE_CODE_CLIENT_KEY"),
        ];
        // `getMTLSConfig` is memoized.
        crate::utils::mtls::clear_mtls_cache();

        assert_eq!(
            build_api_provider_properties(),
            vec![
                property("API provider", "AWS Bedrock"),
                property("Bedrock base URL", "https://bedrock.example"),
                property("AWS region", "us-west-2"),
                unlabeled_property("AWS auth skipped"),
                property("Proxy", "http://lowercase-proxy"),
                property("Additional CA cert(s)", "/tmp/ca.pem"),
                property("mTLS client cert", cert.to_string_lossy()),
            ]
        );

        drop(guards);
        crate::utils::mtls::clear_mtls_cache();
        let _ = std::fs::remove_file(cert);
    }

    #[test]
    fn build_account_properties_matches_official_sources_without_secret_values() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "cometix-status-account-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _config_home = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _oauth = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _api_key = EnvVarGuard::unset("ANTHROPIC_API_KEY");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");

        let mut config = crate::utils::config::GlobalConfig::default();
        config.primary_api_key = Some("sk-ant-secret".to_string());
        config.oauth_account = Some(crate::utils::config::AccountInfo {
            email_address: Some("user@example.com".to_string()),
            organization_name: Some("Example Org".to_string()),
            ..crate::utils::config::AccountInfo::default()
        });
        let previous = crate::utils::config::replace_test_global_config(Some(config));

        let rows = build_account_properties();
        assert_eq!(
            rows,
            vec![
                property("Auth token", "none"),
                property("API key", "/login managed key"),
                property("Organization", "Example Org"),
                property("Email", "user@example.com"),
            ]
        );
        assert!(rows.iter().all(|row| !row.value.contains("sk-ant-secret")));

        crate::utils::config::replace_test_global_config(previous);
        let _ = std::fs::remove_dir_all(dir);
    }
}
