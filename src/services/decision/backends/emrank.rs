//! EMRank provider (decision-provider.md §5.1): the primary backend.
//!
//! @nyan: EMRank serves the System One protocol natively, so everything
//! protocol-shaped lives in [`super::system_one`]. This file only supplies
//! what is EMRank-specific — endpoint, key file, model aliases, pool tuning —
//! and constructs the shared [`SystemOneClient`]. A second System One provider
//! needs the same ~30 lines, not a second client.
//!
//! Endpoint and models (measured 2026-10-27): `POST /v1/decisions`,
//! `typesafe/jev-1.14.0` for the critical path (~0.4 s warm),
//! `cloudflare/clef` for background points. Model IDs are pinned: `latest`
//! moves under the log, which would make `/decision` statistics
//! incomparable across an upgrade.

use std::sync::Arc;

use super::system_one::{NativeSystemOneTransport, SystemOneClient};
use crate::services::service_health::{self, ServiceKind};
use crate::utils::settings::nyan::EmrankSettings;

/// Build the EMRank client from settings.
///
/// Reads the key file (trimmed), so this is called lazily on the first
/// decision, not at startup. Errors carry the path only, never the content.
pub fn from_settings(settings: &EmrankSettings) -> anyhow::Result<Arc<SystemOneClient>> {
    crate::utils::tls_provider::install_crypto_provider();
    let api_key = super::super::read_api_key_file(settings.api_key_file())?;
    let client = build_client(settings.pool_idle_timeout_ms())?;
    Ok(SystemOneClient::with_transport(
        settings.url(),
        api_key,
        client,
        service_health::shared(ServiceKind::Emrank),
        Arc::new(NativeSystemOneTransport),
    ))
}

/// The shared reqwest client: keep-alive pool sized for the decision traffic
/// (§5.2), a short connect budget so a dead endpoint fails inside the
/// critical-path deadline, and the `nyan/<version>` UA.
pub fn build_client(pool_idle_timeout_ms: u64) -> anyhow::Result<reqwest::Client> {
    let (connect_timeout, _) = ServiceKind::Emrank.default_timeouts();
    // Outbound goes through the same proxy construction as external search
    // (§5.2, external-search 4.2 rule 1).
    crate::utils::proxy::get_proxy_fetch_options()?
        .connect_timeout(connect_timeout)
        .pool_idle_timeout(std::time::Duration::from_millis(pool_idle_timeout_ms))
        .pool_max_idle_per_host(4)
        .user_agent(super::system_one::user_agent())
        .build()
        .map_err(|error| anyhow::anyhow!("cannot build the EMRank client: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_settings_point_at_emrank_with_pinned_models() {
        let settings = EmrankSettings::default();
        assert_eq!(settings.url(), "https://emrank.tech/v1/decisions");
        assert_eq!(settings.api_key_file(), "~/.nyan/keys/emrank");
        assert_eq!(
            settings.model_id(crate::utils::settings::nyan::DecisionModel::Fast),
            "typesafe/jev-1.14.0",
            "latest would move under the log"
        );
        assert_eq!(
            settings.model_id(crate::utils::settings::nyan::DecisionModel::Accurate),
            "cloudflare/clef"
        );
        assert_eq!(settings.pool_idle_timeout_ms(), 90_000);
        assert!(settings.prewarm());
    }

    #[test]
    fn the_model_alias_table_is_overridable_per_alias() {
        let mut settings = EmrankSettings::default();
        settings.models = Some(
            [("fast".to_string(), "typesafe/jev-1.15.0".to_string())]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            settings.model_id(crate::utils::settings::nyan::DecisionModel::Fast),
            "typesafe/jev-1.15.0"
        );
        assert_eq!(
            settings.model_id(crate::utils::settings::nyan::DecisionModel::Accurate),
            "cloudflare/clef",
            "unset alias keeps the built-in"
        );
    }

    #[test]
    fn building_without_a_key_file_fails_without_leaking_anything() {
        let mut settings = EmrankSettings::default();
        settings.api_key_file = Some("/nonexistent/nyan/emrank-key".to_string());
        let error = from_settings(&settings).unwrap_err();
        assert!(error.to_string().contains("/nonexistent/nyan/emrank-key"));
    }
}
