//! @nyan: Google Gemini adapter (provider redesign PR 6).
//!
//! Handles `ProviderProtocol::Gemini`. Maps request IR to Gemini
//! `GenerateContentRequest`, handles API key authentication, parses
//! Server-Sent Events into IR stream events, and maps Google RPC error statuses.

pub mod adapter;
pub mod errors;
pub mod policy;
pub mod target;
pub mod wire;

#[cfg(test)]
pub mod fixtures;
#[cfg(test)]
pub mod mock;

use crate::services::provider::adapter::{Cancel, OpenedStream, ProviderAdapter, ResolvedTarget};
use crate::services::provider::ir::{ModelRequest, ProviderError, ProviderErrorKind};
use crate::utils::model::profile::ResolvedModelProfile;

/// Open one streaming attempt for a Gemini-protocol profile: resolve its
/// credential, check the request against what the profile declares, send.
pub async fn open_stream(
    profile: &ResolvedModelProfile,
    request: &ModelRequest,
    cancel: &Cancel,
) -> Result<OpenedStream, ProviderError> {
    let invalid = |message: String| ProviderError::new(ProviderErrorKind::InvalidRequest, message);
    let target = target::target_for_profile(profile)
        .await
        .map_err(|error| invalid(error.to_string()))?;
    let adapter = adapter::GeminiAdapter::new(target);
    let resolved = ResolvedTarget::from_profile(profile);
    adapter
        .validate(&resolved, request)
        .map_err(|error| invalid(error.to_string()))?;
    adapter.send(&resolved, request, cancel).await
}

/// Test support: install a Gemini main profile in the session settings cache,
/// pointing to `base_url` with key held in `env_name`.
#[cfg(test)]
pub(crate) fn install_test_main_profile(
    base_url: &str,
    env_name: &str,
    capabilities: crate::utils::settings::nyan::ModelCapabilities,
) {
    use crate::utils::settings::SettingsJson;
    use crate::utils::settings::nyan::{
        CredentialReference, ModelProfileSettings, NyanSettings, ProviderKind, ProviderProtocol,
        ProviderSettings, SelectionSettings,
    };
    use crate::utils::settings::settings_cache::set_session_settings_cache;
    use crate::utils::settings::validation::SettingsWithErrors;
    use std::collections::BTreeMap;

    let nyan = NyanSettings {
        providers: Some(BTreeMap::from([(
            "google".to_string(),
            ProviderSettings {
                kind: Some(ProviderKind::Gemini),
                protocols: Some(vec![ProviderProtocol::Gemini]),
                base_url: Some(base_url.to_string()),
                credential: Some(CredentialReference {
                    env: Some(env_name.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )])),
        models: Some(BTreeMap::from([(
            "main".to_string(),
            ModelProfileSettings {
                provider: Some("google".to_string()),
                model: Some("gemini-2.5-flash".to_string()),
                protocol: Some(ProviderProtocol::Gemini),
                capabilities: Some(capabilities),
                ..Default::default()
            },
        )])),
        selection: Some(SelectionSettings {
            main: Some("main".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    set_session_settings_cache(SettingsWithErrors {
        settings: SettingsJson {
            nyan: Some(Box::new(nyan)),
            ..Default::default()
        },
        ..Default::default()
    });
}
