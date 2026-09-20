//! Trait abstraction for proxy channels (CPA, BPA, DPA, etc.).

use anyhow::Result;
use futures::future::BoxFuture;

use super::types::{DiscoveredModel, Endpoints};

/// Contract that every proxy channel implementation must satisfy.
pub trait ProxyChannel: Send + Sync {
    /// Unique channel identifier, e.g. "cpa", "bpa", "other".
    fn id(&self) -> &'static str;

    /// Human-readable display name, e.g. "CLIProxyAPI".
    fn display_name(&self) -> &'static str;

    /// Default base URL used as suggestion in interactive setup.
    fn default_base_url(&self) -> &'static str;

    /// Normalize an arbitrary user-input base URL into standard inference and models endpoints.
    fn normalize_endpoints(&self, input: &str) -> Result<Endpoints>;

    /// Fetch the remote model catalog from the proxy.
    fn fetch_models<'a>(
        &'a self,
        ep: &'a Endpoints,
        api_key: &'a str,
    ) -> BoxFuture<'a, Result<Vec<DiscoveredModel>>>;
}
