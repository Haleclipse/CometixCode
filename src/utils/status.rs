//! Maps to: CC `utils/status.tsx`.

use crate::utils::process_env::JsTruthy;
use crate::utils::settings::constants::get_setting_source_display_name_capitalized;
use crate::utils::settings::{SettingsJson, get_enabled_setting_sources, get_settings_for_source};

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
    /// Nyan does not run installer probes from the main-screen UI.
    pub installation_diagnostics: Vec<String>,
    /// Already-known doctor warnings for Settings/Status diagnostics.
    /// Nyan does not run external doctor probes from the main-screen UI.
    pub doctor_diagnostics: Vec<String>,
    /// Already-known IDE installation status for Settings/Status summaries.
    /// Nyan never installs IDE extensions from Status.
    pub ide_installation_status: Option<crate::utils::ide::IDEExtensionInstallationStatus>,
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

/// Maps to: CC `utils/status.tsx:286-330` `buildAccountProperties`.
///
/// The rows (login method, auth token source, organization, email) all described
/// a claude.ai/Console OAuth login or an ambient `ANTHROPIC_AUTH_TOKEN`. Neither
/// exists: Anthropic credentials come only from the selected provider profile,
/// so there is no account to report.
pub fn build_account_properties() -> Vec<Property> {
    Vec::new()
}

/// Maps to: CC `utils/status.tsx:332-460` `buildAPIProviderProperties`.
pub fn build_api_provider_properties() -> Vec<Property> {
    // Provider credentials and endpoints come only from the selected nyan
    // profile, so there is no provider environment row to report.
    let mut properties = Vec::new();

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
        if let Some(cert_path) =
            truthy("CLAUDE_CODE_CLIENT_CERT").filter(|_| loaded(&mtls_config.cert))
        {
            properties.push(property("mTLS client cert", cert_path));
        }
        if let Some(key_path) =
            truthy("CLAUDE_CODE_CLIENT_KEY").filter(|_| loaded(&mtls_config.key))
        {
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
    fn build_api_provider_properties_matches_official_process_rows() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cert = std::env::temp_dir().join(format!(
            "nyan-status-cert-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&cert, "certificate").unwrap();
        let guards = vec![
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
    fn build_account_properties_reports_no_account_even_with_ambient_credentials() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _oauth = EnvVarGuard::set("CLAUDE_CODE_OAUTH_TOKEN", "ambient-oauth-token");
        let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "sk-ant-secret");

        let mut config = crate::utils::config::GlobalConfig::default();
        config.primary_api_key = Some("sk-ant-secret".to_string());
        config.oauth_account = Some(crate::utils::config::AccountInfo {
            email_address: Some("user@example.com".to_string()),
            organization_name: Some("Example Org".to_string()),
            ..crate::utils::config::AccountInfo::default()
        });
        let previous = crate::utils::config::replace_test_global_config(Some(config));

        assert_eq!(build_account_properties(), Vec::<Property>::new());

        crate::utils::config::replace_test_global_config(previous);
    }
}
