//! Data types for proxy channel services.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Resolved endpoints for a proxy channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoints {
    /// Base URL for Anthropic-compatible inference requests (e.g. `http://127.0.0.1:8317`).
    pub inference_base_url: String,
    /// URL for remote model discovery (e.g. `http://127.0.0.1:8317/v1/models?client_version=cometix`).
    pub models_url: String,
}

/// A discovered model item returned from the proxy's model discovery endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    /// Canonical model id/slug (e.g. "claude-3-7-sonnet" or "gemini-2.5-pro").
    pub id: String,
    /// Display name for the model.
    pub name: String,
    /// Optional description if provided by the catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Contents of `~/.claude/setting_proxy_active.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyActiveConfig {
    /// Identifier of the currently active channel, e.g. `Some("cpa")` or `None` (native).
    pub active: Option<String>,
}

/// Contents of `~/.claude/setting_<id>.json`.
/// Follows Style B: an `env` override block matching `settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelConfigFile {
    /// Environment variable overrides to apply when this channel is active.
    #[serde(default)]
    pub env: IndexMap<String, String>,
}

/// Contents of `~/.claude/setting_<id>_models.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelModelsCache {
    /// Base URL with which this cache was queried.
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    /// Unix timestamp in milliseconds when models were fetched.
    #[serde(rename = "fetchedAt")]
    pub fetched_at: u64,
    /// List of models discovered from the channel.
    #[serde(default)]
    pub models: Vec<DiscoveredModel>,
}
