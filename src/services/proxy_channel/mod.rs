//! Multi-channel proxy services (CLIProxyAPI, BPA, DPA, etc.).
//!
//! Provides trait abstraction, endpoint normalization, configuration persistence,
//! and process environment overrides for third-party AI proxy gateways.

pub mod active_env;
pub mod config;
pub mod cpa;
pub mod other;
pub mod traits;
pub mod types;

pub use active_env::{PROXY_MANAGED_ENV_KEYS, apply_active_channel_env, deactivate_active_channel_env};
pub use config::{
    active_proxy_path, channel_config_path, channel_models_cache_path, get_active_channel_id,
    load_channel_config, load_channel_models_cache, save_channel_config, save_channel_models_cache,
    set_active_channel_id, update_channel_env,
};
pub use traits::ProxyChannel;
pub use types::{ChannelConfigFile, ChannelModelsCache, DiscoveredModel, Endpoints, ProxyActiveConfig};

/// Returns the proxy channel implementation matching the given identifier.
pub fn get_channel(id: &str) -> Option<&'static dyn ProxyChannel> {
    match id {
        "cpa" => Some(&cpa::CliProxyApiChannel),
        "other" => Some(&other::OtherProxyChannel),
        _ => None,
    }
}

/// Returns a list of all registered proxy channel implementations.
pub fn all_channels() -> Vec<&'static dyn ProxyChannel> {
    vec![&cpa::CliProxyApiChannel, &other::OtherProxyChannel]
}
