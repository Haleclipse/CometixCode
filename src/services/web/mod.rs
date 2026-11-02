//! External web search/fetch services.
//!
//! @nyan: WebSearch and WebFetch share this module so SSRF policy, snapshots,
//! provider errors, session budgets, and deterministic result rendering cannot
//! drift between the two tools.

pub mod budget;
pub mod decision;
pub mod extract;
pub mod fallback;
pub mod fetcher;
pub mod host_guard;
pub mod provider_health;
pub mod providers;
pub mod redact;
pub mod search;
pub mod snapshot;
#[cfg(test)]
pub(crate) mod test_support;

pub use fetcher::{FetchConfig, FetchError, FetchedPage, RedirectHop, fetch_url};

pub fn settings() -> Option<crate::utils::settings::nyan::WebSettings> {
    crate::utils::settings::get_initial_settings()
        .nyan
        .and_then(|nyan| nyan.web)
}

pub fn is_configured() -> bool {
    settings().is_some()
}

pub fn has_search_provider() -> bool {
    settings().is_some_and(|web| {
        web.providers.as_ref().is_some_and(|providers| {
            providers
                .values()
                .any(|provider| provider.enabled != Some(false))
        })
    })
}

pub fn rerank_client(
    web: &crate::utils::settings::nyan::WebSettings,
) -> Option<crate::services::rerank::RerankClient> {
    let settings = crate::utils::settings::get_initial_settings();
    let rerank = settings
        .nyan
        .as_ref()?
        .retrieval
        .as_ref()?
        .rerank
        .as_ref()?;
    rerank.url.as_ref()?;
    crate::services::rerank::RerankClient::from_settings(rerank, web.search().rerank().timeout_ms())
        .ok()
}
