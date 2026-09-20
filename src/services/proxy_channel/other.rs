//! Generic/Other proxy channel implementation.

use anyhow::Result;
use futures::future::BoxFuture;

use super::cpa::CliProxyApiChannel;
use super::traits::ProxyChannel;
use super::types::{DiscoveredModel, Endpoints};

pub const DEFAULT_OTHER_BASE_URL: &str = "http://127.0.0.1:8000";

pub struct OtherProxyChannel;

impl ProxyChannel for OtherProxyChannel {
    fn id(&self) -> &'static str {
        "other"
    }

    fn display_name(&self) -> &'static str {
        "OtherProxy"
    }

    fn default_base_url(&self) -> &'static str {
        DEFAULT_OTHER_BASE_URL
    }

    fn normalize_endpoints(&self, input: &str) -> Result<Endpoints> {
        // Reuse robust URL normalization
        CliProxyApiChannel::normalize_endpoints_impl(input)
    }

    fn fetch_models<'a>(
        &'a self,
        ep: &'a Endpoints,
        api_key: &'a str,
    ) -> BoxFuture<'a, Result<Vec<DiscoveredModel>>> {
        Box::pin(CliProxyApiChannel::fetch_models_impl(ep, api_key))
    }
}
